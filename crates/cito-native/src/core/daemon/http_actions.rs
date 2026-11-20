use super::{
    channels::{channel_action_kind, dispatch_channel_action},
    host_hub::{
        advertised_hub_host, host_hub_auth_mode, host_hub_server_config, hub_name_from_url,
        lan_advertised_host, parse_hub_invite, replace_host_hub_server, tailscale_advertised_host,
    },
    mcp::validate_tool_input,
    request_shutdown, shutdown_requested,
    ui_ipc::bounded_frame,
    wake_daemon, DaemonService, SHUTDOWN_REQUESTED,
};
use crate::core::{
    config::repair_default_hub,
    identity::{listener_id, local_host_name, normalize_slug, string},
};
use crate::plugins::now_ms;
use crate::setup::{
    apply_plugin_setup, ensure_single_plugin_staged, remove_plugin_setup, set_launch_at_login,
};
use serde_json::{json, Map, Value};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct InteractiveLogin {
    pub(super) cancelled: crate::core::login::Cancellation,
    serial: Mutex<()>,
}

fn login_cache_snapshot(path: &std::path::Path) -> Option<(Value, Value)> {
    let file = fs::File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    let cache = serde_json::from_reader(file).ok()?;
    // Atomic cache publication replaces the inode even when an issuer returns
    // the same bearer bytes. Bind queued login intent to that publication.
    let generation = json!([
        metadata.dev(),
        metadata.ino(),
        metadata.mtime(),
        metadata.mtime_nsec()
    ]);
    Some((cache, generation))
}

mod transport;
pub(super) use transport::{handle_http, respond};

pub(super) fn interactive_login(action: &Value) -> bool {
    action["op"] == "login_hub"
        || (action["op"] == "hub_action"
            && matches!(action["action"].as_str(), Some("login" | "switch_account")))
}

/// Authoritative success survives failures rebuilding a secondary local view.
fn committed_projection(mut result: Value, projection: Result<(), String>) -> Value {
    if result["ok"] != false {
        result["ok"] = json!(true);
        result["committed"] = json!(true);
        if let Err(error) = projection {
            let warning = format!("Saved successfully; local refresh failed: {error}");
            result["warning"] = json!(match result["warning"].as_str() {
                Some(previous) => format!("{previous}; {warning}"),
                None => warning,
            });
        }
    }
    result
}

pub(super) fn dispatch_action(
    service: &Arc<Mutex<DaemonService>>,
    action: &Value,
    ui_generation: Option<u64>,
) -> Result<Value, String> {
    let mut result = dispatch_action_inner(service, action, ui_generation);
    let refresh_setup = matches!(
        string(action, "op"),
        "setup_apply" | "setup_remove_app" | "setup_disconnect_all"
    );
    // Await invalidated inspections without holding service. Retain the final
    // guard so another mutation cannot invalidate them before receipt publication.
    let completion = service
        .lock()
        .map_err(|_| "Daemon lock poisoned")?
        .setup_completion
        .clone();
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut s = loop {
        let before = *completion.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
        if !refresh_setup {
            // Composer actions settle under their original guard, including
            // preflight errors. Do not replace that receipt after re-entry.
            if !matches!(
                string(action, "op"),
                "send_signal" | "send_reply_from_human"
            ) {
                s.validate_ui_generation(ui_generation)?;
            }
            break s;
        }
        s.validate_ui_generation(ui_generation)?;
        if s.closing || shutdown_requested() {
            result = result.map(|value| {
                committed_projection(value, Err("Shutting down before local refresh".into()))
            });
            break s;
        }
        s.refresh_setup_inspections();
        if s.setup_inspections
            .values()
            .all(|entry| entry.worker.is_none())
        {
            break s;
        }
        if Instant::now() >= deadline {
            result = result
                .map(|value| committed_projection(value, Err("Setup inspection timed out".into())));
            break s;
        }
        drop(s);
        let revision = completion.0.lock().unwrap_or_else(|e| e.into_inner());
        let _ = completion
            .1
            .wait_timeout_while(
                revision,
                deadline.saturating_duration_since(Instant::now()),
                |revision| *revision == before,
            )
            .unwrap_or_else(|e| e.into_inner());
    };
    if interactive_login(action)
        || (result.is_err() && matches!(string(action, "op"), "setup_save_hub" | "save_hub"))
    {
        s.record_hub_result(action, &result, None);
    }
    if let Err(error) = &result {
        if channel_action_kind(action) != Some(super::channels::ChannelActionKind::Mutation)
            && !matches!(
                string(action, "op"),
                "set_host_hub" | "send_signal" | "send_reply_from_human"
            )
        {
            s.emit(
            json!({"op":"error","error":error,"source_op":action["op"],"request_id":action["request_id"],"hub":action["hub"]}),
        );
        }
    }
    if refresh_setup && result.is_ok() {
        s.emit(json!({"op":"settings","ok":true,"committed":true,"source_op":action["op"],"request_id":action["request_id"],"settings":s.settings(),"warning":result.as_ref().ok().and_then(|value| value.get("warning"))}));
        s.push_ui_result(Some(action));
    } else {
        s.push_ui();
    }
    drop(s);
    wake_daemon();
    result
}

fn dispatch_action_inner(
    service: &Arc<Mutex<DaemonService>>,
    action: &Value,
    ui_generation: Option<u64>,
) -> Result<Value, String> {
    if let Some(kind) = channel_action_kind(action) {
        return dispatch_channel_action(service, action, kind, ui_generation);
    }
    if matches!(
        string(action, "op"),
        "setup_save_hub" | "save_hub" | "paste_hub_invite"
    ) && action["parse_only"] != true
    {
        let (before, default_hub) = {
            let s = service.lock().map_err(|_| "Daemon lock poisoned")?;
            s.validate_ui_generation(ui_generation)?;
            if action["op"] == "paste_hub_invite" {
                s.reject_own_hub_invite(&parse_hub_invite(string(action, "invite"))?)?;
            }
            (s.raw_config()?, s.config.default_hub.clone())
        };
        let mut prepared = action.clone();
        let submitted = if action["op"] == "paste_hub_invite" {
            parse_hub_invite(string(action, "invite"))?
        } else {
            action.clone()
        };
        let patch = submitted
            .get("config")
            .filter(|v| v.is_object())
            .unwrap_or(&submitted);
        let original = submitted["original_name"]
            .as_str()
            .or_else(|| submitted["hub"].as_str())
            .unwrap_or(&default_hub);
        let base = if submitted["edit"] == true {
            &before["hubs"][original]
        } else {
            &Value::Null
        };
        let config = crate::core::config::discover_hub_config(
            crate::core::config::merge_hub_config(base, patch),
        )?;
        prepared["config"] = config;
        let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
        s.validate_ui_generation(ui_generation)?;
        if s.closing || shutdown_requested() {
            return Err("Shutting down".into());
        }
        return s.action_with_precondition(&prepared, Some(&before));
    }
    let (hub, config, state_dir, attempt, snapshot) = {
        let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
        if matches!(
            string(action, "op"),
            "send_signal" | "send_reply_from_human"
        ) {
            let preflight = s.validate_ui_generation(ui_generation).and_then(|()| {
                if s.closing || shutdown_requested() {
                    Err("Shutting down".into())
                } else {
                    Ok(())
                }
            });
            if let Err(error) = preflight {
                let result = Err(error);
                s.emit_composer_result(action, &result);
                return result;
            }
            return s.action(action);
        }
        s.validate_ui_generation(ui_generation)?;
        if s.closing || shutdown_requested() {
            return Err("Shutting down".into());
        }
        if !interactive_login(action) {
            return s.action(action);
        }
        s.reload_slots()?;
        let hub = action["hub"]
            .as_str()
            .unwrap_or(&s.config.default_hub)
            .to_string();
        s.mutate_config(|v| {
            if !v["hubs"].is_object() || !v["hubs"][&hub].is_object() {
                return Err("INVALID_INPUT: Unknown hub".into());
            }
            v["hubs"][&hub]["enabled"] = json!(true);
            if let Some(m) = v["hubs"][&hub].as_object_mut() {
                m.remove("disabled");
            }
            Ok(())
        })?;
        s.reload_slots()?;
        let config = s.slot(&hub)?.config.clone();
        let snapshot =
            login_cache_snapshot(&s.config.state_dir.join(format!("oidc-token-{hub}.json")));
        let attempt = s.logins.entry(hub.clone()).or_default().clone();
        (hub, config, s.config.state_dir.clone(), attempt, snapshot)
    };
    // Browser callbacks and cache-lock waits never hold the daemon's global mutex.
    // Same-hub UI logins share publication, even when the provider returns
    // identical bearer bytes. Neither this queue nor the browser holds service.
    let _serial = attempt.serial.lock().map_err(|_| "Login lock poisoned")?;
    let result = {
        let current = login_cache_snapshot(&state_dir.join(format!("oidc-token-{hub}.json")));
        let changed = snapshot.as_ref().map(|(_, generation)| generation)
            != current.as_ref().map(|(_, generation)| generation);
        let rejected = if changed {
            None
        } else {
            snapshot
                .as_ref()
                .and_then(|(cache, _)| cache["token_sha256"].as_str())
        };
        crate::core::login::recover_cancellable_with_options(
            &state_dir,
            &hub,
            &config,
            rejected,
            true,
            action["action"] == "switch_account" || action["force_interactive"] == true,
            (
                &|| shutdown_requested() || attempt.cancelled.load(Ordering::SeqCst),
                Some(attempt.cancelled.fd()?),
            ),
        )
    };
    // Release per-Hub publication before re-entering the service mutex. Purge
    // may hold service while cancelling and waiting for this publication lock.
    drop(_serial);
    let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
    let current = s.logins.get(&hub).is_some_and(|c| Arc::ptr_eq(c, &attempt));
    if current && Arc::strong_count(&attempt) == 2 {
        s.logins.remove(&hub);
    }
    if !current || attempt.cancelled.load(Ordering::SeqCst) || s.closing || shutdown_requested() {
        return Err("OIDC_LOGIN_TIMEOUT: OIDC login cancelled".into());
    }
    s.reload_slots()?;
    if !s.slots.get(&hub).is_some_and(|slot| slot.config == config) {
        return Err("OIDC_LOGIN_TIMEOUT: Hub changed during login".into());
    }
    let (token, identity) = result?;
    if let Some(old) = s.slots.remove(&hub) {
        old.close();
    }
    for row in s.state.active()?.into_iter().filter(|row| {
        row["hub"] == hub
            && !crate::core::auth::principal_ids_equal(
                string(row, "principal_id"),
                string(&identity, "principal_id"),
            )
    }) {
        s.state.fence_listener_for(&row, "fenced_before_dispatch")?;
    }
    s.reload_slots()?;
    s.push_ui_result(Some(action));
    Ok(
        json!({"ok":true,"committed":true,"user":identity["owner_user"],"authenticated":!token.is_empty()}),
    )
}

pub(super) fn mutating_action(action: &Value) -> bool {
    matches!(
        string(action, "op"),
        "setup_save_hub"
            | "save_hub"
            | "set_host_hub"
            | "setup_apply"
            | "setup_remove_app"
            | "setup_disconnect_all"
            | "setup_remove_hub"
            | "remove_hub"
            | "setup_toggle_hub"
            | "hub_action"
            | "set_user"
            | "switch_hub"
            | "set_launch_at_login"
            | "set_auto_listen"
            | "set_scope"
            | "set_listen_mode"
            | "human_inbox_update"
            | "listen_session"
            | "unlisten"
            | "rename_session"
            | "cli_rename"
            | "cli_human_device"
            | "login_hub"
            | "unlisten_session"
            | "unlisten_all"
            | "ack_signals"
            | "mark_activity_read"
            | "mark_all_activity_read"
            | "clear_activity_history"
    )
}

impl DaemonService {
    fn validate_configured_hub(&self, hub: &str) -> Result<(), String> {
        let (_, hubs, _) =
            crate::plugins::try_normalize_existing_config(Some(&self.raw_config()?.to_string()))?;
        if hubs.get(hub).is_some_and(|cfg| cfg["enabled"] != false) {
            Ok(())
        } else {
            Err("INVALID_INPUT: Unknown or disabled hub".into())
        }
    }
    pub(super) fn purge_hub_state(&mut self, hub: &str) -> Result<(), String> {
        if let Some(attempt) = self.logins.remove(hub) {
            attempt.cancelled.store(true, Ordering::SeqCst);
            drop(attempt.serial.lock().map_err(|_| "Login lock poisoned")?);
        }
        if let Some(slot) = self.slots.remove(hub) {
            slot.close();
        }
        if let Some(pending) = self.oidc_recoveries.remove(hub) {
            pending.cancelled.store(true, Ordering::SeqCst);
            let _ = pending.worker.join();
        }
        // Retired recoveries were cancelled when their profile changed. Join
        // them before credential deletion so a final publication cannot outlive purge.
        for pending in self.retired_auth.drain(..) {
            pending.cancelled.store(true, Ordering::SeqCst);
            let _ = pending.worker.join();
        }
        let belongs = |key: &str| {
            key.starts_with(&format!("{hub}:"))
                || ["directory", "channels", "auto"]
                    .iter()
                    .any(|prefix| key == format!("{prefix}:{hub}"))
        };
        let workers: Vec<_> = self
            .receivers
            .keys()
            .filter(|key| belongs(key))
            .cloned()
            .collect();
        for key in workers {
            if let Some(worker) = self.receivers.remove(&key) {
                let _ = worker.join();
            }
        }
        self.state.db.transaction(|db| {
            for table in [
                "local_signals",
                "local_listeners",
                "pending_hub_acks",
                "pending_hub_detaches",
                "local_sent_idempotency",
                "wake_retries",
            ] {
                db.execute(&format!("DELETE FROM {table} WHERE hub=?"), &[json!(hub)])?;
            }
            Ok(())
        })?;
        self.last_hub_errors.remove(hub);
        self.auth_backoff.remove(hub);
        self.channel_inventories.remove(hub);
        self.observations.retain(|key, _| !belongs(key));
        if let Ok(mut retries) = self.ack_backoff.lock() {
            retries.retain(|key, _| {
                serde_json::from_str::<Value>(key).is_ok_and(|value| value[0] != hub)
            });
        }
        let token = self.config.state_dir.join(format!("oidc-token-{hub}.json"));
        match fs::remove_file(token) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn reject_own_hub_invite(&self, invite: &Value) -> Result<(), String> {
        let Some(server) = &self.host_server else {
            return Ok(());
        };
        let url =
            url::Url::parse(string(invite, "url")).map_err(|_| "INVALID_INPUT: Invalid Hub URL")?;
        let raw = self.raw_config()?;
        let host = raw
            .get("host_hub")
            .or_else(|| raw.get("hostHub"))
            .unwrap_or(&Value::Null);
        let bind = string(host, "bind");
        let hostname = local_host_name();
        let mut local_hosts = vec![
            "127.0.0.1".into(),
            "localhost".into(),
            "::1".into(),
            hostname.clone(),
            format!("{}.local", hostname.trim_end_matches(".local")),
            advertised_hub_host(bind),
            lan_advertised_host(bind),
        ];
        if matches!(bind, "" | "0.0.0.0" | "::") {
            local_hosts.extend(tailscale_advertised_host());
        }
        let invite_host = url
            .host_str()
            .unwrap_or("")
            .trim_matches(['[', ']'])
            .trim_end_matches('.');
        if url.port_or_known_default() == Some(server.port())
            && local_hosts.iter().any(|local| {
                local
                    .trim_matches(['[', ']'])
                    .trim_end_matches('.')
                    .eq_ignore_ascii_case(invite_host)
            })
        {
            return Err("INVALID_INPUT: This invite code belongs to this Mac's own shared Hub (already active as 'local'). Paste it in Cito on your other Mac to connect.".into());
        }
        Ok(())
    }
    pub fn action(&mut self, action: &Value) -> Result<Value, String> {
        self.action_with_precondition(action, None)
    }

    fn action_with_precondition(
        &mut self,
        action: &Value,
        precondition: Option<&Value>,
    ) -> Result<Value, String> {
        let mut resolved_hub = None;
        let result = self
            .action_inner(action, &mut resolved_hub, precondition)
            .map(|mut value| {
                if mutating_action(action) && value["ok"] != false {
                    value["ok"] = json!(true);
                    value["committed"] = json!(true);
                }
                if let Some(name) = &resolved_hub {
                    value["redirect_port"] = self
                        .slots
                        .get(name)
                        .map(|slot| slot.config["redirectPort"].clone())
                        .unwrap_or(Value::Null);
                }
                value
            });
        if matches!(
            string(action, "op"),
            "send_signal" | "send_reply_from_human"
        ) {
            self.emit_composer_result(action, &result);
        }
        self.record_hub_result(action, &result, resolved_hub.as_deref());
        if action["op"] == "set_host_hub" {
            if let Err(error) = &result {
                self.emit(json!({"op":"settings","ok":false,"source_op":"set_host_hub","request_id":action["request_id"],"error":error,"settings":self.settings()}));
            }
        }
        if result.is_ok() && matches!(string(action, "op"), "setup_save_hub" | "save_hub") {
            self.emit(json!({"op":"settings","ok":true,"committed":true,"source_op":action["op"],"request_id":action["request_id"],"settings":self.settings(),"warning":result.as_ref().ok().and_then(|value| value.get("warning"))}));
        }
        if !matches!(
            string(action, "op"),
            "setup_apply" | "setup_remove_app" | "setup_disconnect_all"
        ) {
            if let Some(warning) = result
                .as_ref()
                .ok()
                .and_then(|value| {
                    value
                        .get("warning")
                        .or_else(|| value.get("maintenance_warning"))
                })
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                self.emit(json!({"op":"state","ok":true,"committed":true,"source_op":action["op"],"request_id":action["request_id"],"warning":warning}));
            }
        }
        if result
            .as_ref()
            .is_ok_and(|value| value["committed"] == true)
            && !matches!(
                string(action, "op"),
                "setup_apply" | "setup_remove_app" | "setup_disconnect_all"
            )
        {
            self.push_ui_result(Some(action));
        } else {
            self.push_ui();
        }
        result
    }

    fn emit_composer_result(&self, action: &Value, result: &Result<Value, String>) {
        self.emit(match result {
            Ok(value) => {
                let mut frame = json!({"op":"composer_result","ok":true,"error":null,"request_id":action["request_id"]});
                if let Some(warning) = value["maintenance_warning"].as_str().filter(|s| !s.is_empty()) {
                    frame["warning"] = json!(warning);
                }
                frame
            },
            Err(error) => json!({"op":"composer_result","ok":false,"error":error,"request_id":action["request_id"]}),
        });
    }

    fn record_hub_result(
        &mut self,
        action: &Value,
        result: &Result<Value, String>,
        resolved_hub: Option<&str>,
    ) {
        if matches!(
            string(action, "op"),
            "login_hub"
                | "hub_action"
                | "test_hub"
                | "setup_save_hub"
                | "save_hub"
                | "setup_toggle_hub"
        ) {
            let saving = matches!(string(action, "op"), "setup_save_hub" | "save_hub");
            let mut hubs = Vec::new();
            if let Some(hub) = action["hub"]
                .as_str()
                .or_else(|| action["name"].as_str())
                .filter(|hub| !hub.is_empty())
            {
                hubs.push(hub.to_owned());
            } else if !saving {
                hubs.push(self.config.default_hub.clone());
            }
            if saving {
                if let Some(hub) = resolved_hub {
                    hubs.push(hub.to_owned());
                }
                let original = string(action, "original_name");
                if !original.is_empty() {
                    hubs.push(original.to_owned());
                }
                let requested = string(action, "name").trim();
                if requested.chars().any(|c| c.is_ascii_alphanumeric()) {
                    hubs.push(normalize_slug(requested));
                }
            }
            hubs.sort_unstable();
            hubs.dedup();
            for hub in hubs {
                match result {
                    Ok(_) => {
                        self.last_hub_errors.remove(&hub);
                    }
                    Err(error) => {
                        let transient = ["NATS_UNAVAILABLE:", "AUTH_UNAVAILABLE:", "UNAUTHORIZED:"]
                            .iter()
                            .any(|prefix| error.starts_with(prefix));
                        self.last_hub_errors.insert(hub, (error.clone(), transient));
                    }
                }
            }
        }
    }

    fn action_inner(
        &mut self,
        action: &Value,
        resolved_hub: &mut Option<String>,
        precondition: Option<&Value>,
    ) -> Result<Value, String> {
        if !action.is_object() {
            return Err("INVALID_INPUT: Action must be an object".into());
        }
        let op = string(action, "op");
        if matches!(op, "unlisten" | "unlisten_session" | "unlisten_all") {
            if action
                .get("hub")
                .is_some_and(|v| !v.is_null() && !v.is_string())
            {
                return Err("INVALID_INPUT: hub must be a string".into());
            }
            if action["harness"] == "human" && action["session_id"] != "default" {
                return Err("INVALID_INPUT: Human session must be default".into());
            }
        }
        let hub = action["hub"]
            .as_str()
            .unwrap_or(&self.config.default_hub)
            .to_string();
        if matches!(
            op,
            "setup_remove_hub" | "remove_hub" | "save_hub" | "setup_save_hub"
        ) || (op == "hub_action" && action["action"] == "disconnect")
            || (op == "setup_toggle_hub" && action["enabled"] == false)
        {
            if self.active_settings_hub.as_deref() == Some(&hub) {
                self.active_settings_hub = None;
            }
            if let Some(cancelled) = self.logins.remove(&hub) {
                cancelled.cancelled.store(true, Ordering::SeqCst);
                drop(cancelled.serial.lock().map_err(|_| "Login lock poisoned")?);
            }
        }
        self.reload_slots()?;
        match op {
            "get_snapshot" => {
                self.refresh_available()?;
                self.observations
                    .retain(|key, _| !key.starts_with("directory:"));
                wake_daemon();
                return self.snapshot();
            }
            "open_menu" => {
                self.menu_open = true;
                self.refresh_available()?;
            }
            "close_menu" => {
                self.menu_open = false;
            }
            "cli_human_device" => {
                if action["attach"] == true {
                    let slot = self.slot(&hub)?;
                    return self.listen(&hub,&json!({"about":"Human"}),&json!({"harness":"human","session_id":"default","cwd":"","title":"Human","principal_id":slot.identity["principal_id"]}));
                }
                let id = self.session_node_id(&hub, "human", "default")?;
                let removed = self.unlisten_id(&hub, &id)?;
                return Ok(json!({"ok":true,"removed":u8::from(removed)}));
            }
            "human_inbox_update" => {
                let slot = self.slot(&hub)?;
                let registration = self.state.registration_lock(&hub)?;
                let _registration = registration
                    .lock()
                    .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
                let id = slot
                    .cached_directory()
                    .into_iter()
                    .find(|row| {
                        row["harness"] == "human"
                            && crate::core::auth::principal_ids_equal(
                                string(row, "principal_id"),
                                string(&slot.identity, "principal_id"),
                            )
                    })
                    .map(|row| string(&row, "listener_id").to_owned())
                    .unwrap_or_else(|| {
                        listener_id(string(&slot.identity, "principal_id"), "human", "default")
                    });
                let mut params = action.clone();
                params["node_id"] = json!(id);
                if params
                    .get("expected_control_revision")
                    .is_none_or(Value::is_null)
                {
                    params["expected_control_revision"] = slot
                        .kv_get(&id)?
                        .map(|(row, _)| row["human_control_revision"].clone())
                        .filter(|v| v.is_number())
                        .unwrap_or(json!(1));
                }
                let result = slot.human_request("human_inbox_update", &params)?;
                let projection = Self::reconcile_human_scope(
                    &self.state,
                    slot,
                    &self.host_id,
                    &result["record"],
                );
                return Ok(committed_projection(result, projection));
            }
            "cli_rename" => {
                let caller = self
                    .caller(&action["meta"], true)?
                    .ok_or("BINDING_REQUIRED: Caller required")?;
                return self.rename_session(
                    string(&caller, "harness"),
                    string(&caller, "session_id"),
                    string(action, "title"),
                );
            }
            "rename_session" => {
                let mut known = self.state.db.query(
                    "SELECT * FROM local_listeners WHERE status!='FENCED_INACTIVE'",
                    &[],
                )?;
                known.extend(
                    self.session_activity
                        .as_ref()
                        .into_iter()
                        .flat_map(|a| a.candidates()),
                );
                let row = known
                    .into_iter()
                    .find(|r| {
                        (action["harness"].is_null() || action["harness"] == r["harness"])
                            && (action["session_id"].is_null()
                                || action["session_id"] == r["session_id"])
                            && (action["listener_id"].is_null()
                                || action["listener_id"] == r["listener_id"])
                            && ((action["harness"].is_string() && action["session_id"].is_string())
                                || action["listener_id"].is_string())
                    })
                    .ok_or("LISTENER_NOT_FOUND: Session is unknown")?;
                return self.rename_session(
                    string(&row, "harness"),
                    string(&row, "session_id"),
                    string(action, "title"),
                );
            }
            "unlisten_session" => {
                if !action["hub"].is_null() {
                    self.validate_configured_hub(&hub)?;
                }
                let mut detach_errors = Vec::new();
                if action["harness"] != "human" {
                    let targets: Vec<String> = self
                        .session_hubs(string(action, "harness"), string(action, "session_id"))?
                        .into_iter()
                        .filter(|name| action["hub"].is_null() || *name == hub)
                        .collect();
                    for name in targets {
                        self.ensure_agent_stop_row(
                            &name,
                            string(action, "harness"),
                            string(action, "session_id"),
                        )?;
                        let slot = self.slots.get(&name);
                        let rows=self.state.db.query("SELECT * FROM local_listeners WHERE hub=? AND harness=? AND session_id=? AND status!='FENCED_INACTIVE'",&[json!(name),action["harness"].clone(),action["session_id"].clone()])?;
                        for row in rows.into_iter().filter(|row| {
                            slot.is_none_or(|slot| {
                                crate::core::auth::principal_ids_equal(
                                    string(row, "principal_id"),
                                    string(&slot.identity, "principal_id"),
                                )
                            })
                        }) {
                            if let Err(error) = self.unlisten_id(&name, string(&row, "listener_id"))
                            {
                                detach_errors.push(format!("{name}: {error}"));
                            }
                        }
                    }
                }
                if action["harness"] == "human" && action["session_id"] == "default" {
                    // Fence automatic enrollment that has not committed a local row yet.
                    for name in self.session_hubs("human", "default")? {
                        if !action["hub"].is_null() && name != hub {
                            continue;
                        }
                        let id = self.session_node_id(&name, "human", "default")?;
                        if let Err(error) = self.unlisten_id(&name, &id) {
                            detach_errors.push(format!("{name}: {error}"));
                        }
                    }
                }
                if !detach_errors.is_empty() {
                    return Err(format!("DETACH_PENDING: {}", detach_errors.join("; ")));
                }
            }
            "set_user" => {
                let user = string(action, "user");
                if !crate::plugins::validate_user(user) {
                    return Err("INVALID_INPUT: Invalid user".into());
                }
                self.mutate_config(|v| {
                    v["user"] = json!(user);
                    Ok(())
                })?;
            }
            "setup_apply" => {
                let id = action["harness"]
                    .as_str()
                    .or_else(|| action["app"].as_str())
                    .ok_or("INVALID_INPUT: harness required")?;
                let p = self
                    .plugins
                    .find_by_id(id)
                    .ok_or("UNSUPPORTED_HARNESS: Unknown harness")?;
                ensure_single_plugin_staged(&self.config.state_dir, p)?;
                let result = apply_plugin_setup(p, true, false);
                self.invalidate_setup(id);
                result?;
                self.last_app_action.insert(id.to_owned(), "setup".into());
                let projection = self.refresh_available();
                return Ok(committed_projection(json!({"ok":true}), projection));
            }
            "setup_remove_app" => {
                let id = action["harness"]
                    .as_str()
                    .or_else(|| action["app"].as_str())
                    .ok_or("INVALID_INPUT: harness required")?;
                let p = self
                    .plugins
                    .find_by_id(id)
                    .ok_or("UNSUPPORTED_HARNESS: Unknown harness")?;
                let result = remove_plugin_setup(p);
                let notice = p
                    .post_disconnect_hint
                    .clone()
                    .unwrap_or("Disconnected · Active listeners stopped".into());
                self.invalidate_setup(id);
                result?;
                let mut detach_errors = Vec::new();
                for row in self
                    .state
                    .active()?
                    .into_iter()
                    .filter(|r| r["harness"] == id)
                {
                    if let Err(error) = self.retire_id(
                        string(&row, "hub"),
                        string(&row, "listener_id"),
                        "disconnected",
                    ) {
                        detach_errors.push(error);
                    }
                }
                self.caller_cache
                    .lock()
                    .map_err(|_| "Caller cache lock poisoned")?
                    .retain(|_, v| v["harness"] != id);
                self.last_app_action
                    .insert(id.to_owned(), "disconnect".into());
                if let Some(activity) = self.session_activity.as_mut() {
                    activity.remove_candidates(id);
                }
                let mut result = json!({"ok":true,"notice":notice,"detach_status":if detach_errors.is_empty(){"complete"}else{"pending"},"warnings":detach_errors});
                if !detach_errors.is_empty() {
                    result["warning"] = json!(detach_errors.join("; "));
                }
                return Ok(result);
            }
            "setup_save_hub" | "save_hub" => {
                let requested_name = string(action, "name").trim();
                if !requested_name.is_empty()
                    && !requested_name.chars().any(|c| c.is_ascii_alphanumeric())
                {
                    return Err("INVALID_INPUT: Invalid hub name".into());
                }
                let mut cfg = action["config"]
                    .as_object()
                    .cloned()
                    .map(Value::Object)
                    .unwrap_or_else(|| action.clone());
                let editing = action["edit"] == true;
                let original = action["original_name"].as_str().unwrap_or(&hub).to_string();
                let mut rename_before = Value::Null;
                let mut renamed_config = Value::Null;
                self.mutate_config(|v| {
                    if precondition.is_some_and(|before| before != v) {
                        return Err(
                            "INVALID_INPUT: Configuration changed during discovery; retry".into(),
                        );
                    }
                    rename_before = v.clone();
                    // Materialize the implicit local Hub before saving/editing a
                    // first explicit Hub, so first-run edits cannot discard it.
                    if v["hubs"].is_null() {
                        v["hubs"] = json!({"local":{"url":"local://"}});
                    }
                    if !v["hubs"].is_object() {
                        return Err("INVALID_INPUT: Hubs must be an object".into());
                    }
                    if editing {
                        let mut base = v["hubs"][&original].clone();
                        if !base.is_object() {
                            return Err("INVALID_INPUT: Unknown hub to edit".into());
                        }
                        if precondition.is_none() {
                            base = crate::core::config::merge_hub_config(&base, &cfg);
                            cfg = base;
                        }
                    }
                    cfg = crate::core::config::merge_hub_config(&Value::Null, &cfg);
                    cfg["url"] = json!(crate::plugins::validate_nats_url(
                        string(&cfg, "url").trim()
                    )
                    .map_err(|error| format!("INVALID_INPUT: {error}"))?);
                    let base_name = if requested_name.is_empty() {
                        hub_name_from_url(string(&cfg, "url"))
                    } else {
                        normalize_slug(requested_name)
                    };
                    let mut name = base_name.clone();
                    if requested_name.is_empty() {
                        let mut suffix = 2;
                        while name == "local"
                            || v["hubs"].get(&name).is_some_and(|existing| {
                                (!editing || name != original) && existing["url"] != cfg["url"]
                            })
                        {
                            let ending = format!("-{suffix}");
                            name = format!(
                                "{}{}",
                                base_name
                                    .chars()
                                    .take(32 - ending.len())
                                    .collect::<String>()
                                    .trim_end_matches('-'),
                                ending
                            );
                            suffix += 1;
                        }
                    }
                    if !crate::plugins::validate_hub_name(&name) {
                        return Err("INVALID_INPUT: Invalid hub name".into());
                    }
                    if name == "local" && cfg["url"] != "local://" {
                        return Err("INVALID_INPUT: The local hub must use local://".into());
                    }
                    if editing && original == "local" && name != "local" {
                        return Err("INVALID_INPUT: Cannot rename the local hub".into());
                    }
                    if !editing
                        && (name == "local"
                            || v["hubs"]
                                .get(&name)
                                .is_some_and(|existing| existing["url"] != cfg["url"]))
                    {
                        return Err("INVALID_INPUT: Hub name already exists".into());
                    }
                    let auth = cfg
                        .get("authMode")
                        .and_then(Value::as_str)
                        .unwrap_or_else(|| {
                            if !string(&cfg, "token").is_empty() {
                                "token"
                            } else if !string(&cfg, "issuer").is_empty()
                                || !string(&cfg, "clientId").is_empty()
                            {
                                "oidc"
                            } else {
                                "none"
                            }
                        })
                        .to_ascii_lowercase();
                    if !matches!(auth.as_str(), "none" | "token" | "oidc") {
                        return Err("INVALID_INPUT: Invalid authMode".into());
                    }
                    cfg["authMode"] = json!(auth);
                    if auth == "token"
                        && cfg["token"]
                            .as_str()
                            .is_none_or(|token| token.trim().is_empty())
                    {
                        return Err("INVALID_INPUT: Hub token is required in token mode".into());
                    }
                    if auth == "oidc" {
                        crate::core::login::Settings::new(&cfg)?;
                    }
                    if cfg["user"].is_null() {
                        if let Some(m) = cfg.as_object_mut() {
                            m.remove("user");
                        }
                    }
                    let set_default = action["set_default"]
                        .as_bool()
                        .or_else(|| cfg["set_default"].as_bool())
                        .unwrap_or(false);
                    if let Some(m) = cfg.as_object_mut() {
                        if let Some(disabled) = m.remove("disabled") {
                            let enabled = m.get("enabled").and_then(Value::as_bool) != Some(false)
                                && disabled.as_bool() != Some(true);
                            m.insert("enabled".to_string(), json!(enabled));
                        }
                        for k in [
                            "op",
                            "name",
                            "hub",
                            "set_default",
                            "edit",
                            "original_name",
                            "request_id",
                        ] {
                            m.remove(k);
                        }
                    }
                    if editing && original != name {
                        if self
                            .config
                            .state_dir
                            .join(format!("oidc-token-{name}.json"))
                            .exists()
                        {
                            return Err(
                                "INVALID_INPUT: Destination Hub credentials already exist".into()
                            );
                        }
                        if !v["hubs"][&name].is_null() {
                            return Err("INVALID_INPUT: Hub name already exists".into());
                        }
                        for table in [
                            "local_listeners",
                            "local_signals",
                            "pending_hub_acks",
                            "pending_hub_detaches",
                            "local_sent_idempotency",
                            "wake_retries",
                        ] {
                            if !self
                                .state
                                .db
                                .query(
                                    &format!("SELECT 1 FROM {table} WHERE hub=? LIMIT 1"),
                                    &[json!(name)],
                                )?
                                .is_empty()
                            {
                                return Err(
                                    "INVALID_INPUT: Destination Hub name has retained local data"
                                        .into(),
                                );
                            }
                        }
                        if let Some(h) = v["hubs"].as_object_mut() {
                            h.remove(&original);
                        }
                        let default = v
                            .get("default_hub")
                            .or_else(|| v.get("defaultHub"))
                            .and_then(Value::as_str);
                        if default == Some(original.as_str()) {
                            v["default_hub"] = json!(name);
                            if let Some(m) = v.as_object_mut() {
                                m.remove("defaultHub");
                            }
                        }
                    }
                    // Retain the identity resolved under the config lock without
                    // adding an internal bookkeeping field to the public response.
                    *resolved_hub = Some(name.clone());
                    renamed_config = cfg.clone();
                    v["hubs"][&name] = cfg;
                    if set_default {
                        v["default_hub"] = json!(name);
                        if let Some(m) = v.as_object_mut() {
                            m.remove("defaultHub");
                        }
                    }
                    Ok(())
                })?;
                if let Some(name) = resolved_hub
                    .as_ref()
                    .filter(|name| editing && **name != original)
                {
                    if let Some(attempt) = self.logins.remove(&original) {
                        attempt.cancelled.store(true, Ordering::SeqCst);
                        drop(attempt.serial.lock().map_err(|_| "Login lock poisoned")?);
                    }
                    if let Some(old) = self.slots.remove(&original) {
                        old.close();
                    }
                    let keys: Vec<_> = self
                        .receivers
                        .keys()
                        .filter(|key| {
                            key.starts_with(&format!("{original}:"))
                                || **key == format!("auto:{original}")
                        })
                        .cloned()
                        .collect();
                    for key in keys {
                        if let Some(worker) = self.receivers.remove(&key) {
                            let _ = worker.join();
                        }
                    }
                    if let Some(pending) = self.oidc_recoveries.remove(&original) {
                        pending.cancelled.store(true, Ordering::SeqCst);
                        let _ = pending.worker.join();
                    }
                    for pending in self.retired_auth.drain(..) {
                        pending.cancelled.store(true, Ordering::SeqCst);
                        let _ = pending.worker.join();
                    }
                    let old_token = self
                        .config
                        .state_dir
                        .join(format!("oidc-token-{original}.json"));
                    let new_token = self
                        .config
                        .state_dir
                        .join(format!("oidc-token-{name}.json"));
                    let migration = self.state.db.transaction(|db| {
                        for table in [
                            "local_listeners",
                            "local_signals",
                            "pending_hub_acks",
                            "pending_hub_detaches",
                            "local_sent_idempotency",
                            "wake_retries",
                        ] {
                            db.execute(
                                &format!("UPDATE {table} SET hub=? WHERE hub=?"),
                                &[json!(name), json!(original)],
                            )?;
                        }
                        if old_token.exists() {
                            fs::rename(&old_token, &new_token).map_err(|e| e.to_string())?;
                        }
                        Ok(())
                    });
                    if let Err(error) = migration {
                        if !old_token.exists() && new_token.exists() {
                            fs::rename(&new_token,&old_token).map_err(|recovery|format!("Hub rename failed: {error}; credential rollback failed: {recovery}"))?;
                        }
                        self.mutate_config(|v| {
                            if v["hubs"][name] != renamed_config { return Err("Hub rename rollback conflicts with a newer configuration".into()); }
                            if let Some(hubs) = v["hubs"].as_object_mut() { hubs.remove(name); if !rename_before["hubs"][&original].is_null() { hubs.insert(original.clone(),rename_before["hubs"][&original].clone()); } }
                            if v["default_hub"] == *name { v["default_hub"] = rename_before.get("default_hub").or_else(||rename_before.get("defaultHub")).cloned().unwrap_or(json!("local")); }
                            Ok(())
                        }).map_err(|recovery|format!("Hub rename failed: {error}; configuration rollback failed: {recovery}"))?;
                        return Err(error);
                    }
                    self.channel_inventories.remove(&original);
                }
            }
            "setup_toggle_hub" | "hub_action" => {
                let enabled = if op == "setup_toggle_hub" {
                    action["enabled"].as_bool().unwrap_or(true)
                } else {
                    string(action, "action") != "disconnect"
                };
                if op == "hub_action" && action["action"] == "reconnect" {
                    if let Some(old) = self.slots.remove(&hub) {
                        old.close();
                    }
                }
                self.mutate_config(|v| {
                    if v["hubs"].is_null() {
                        v["hubs"] = json!({});
                    }
                    if !v["hubs"].is_object() {
                        return Err("INVALID_INPUT: Hubs must be an object".into());
                    }
                    if v["hubs"][&hub].is_null() {
                        v["hubs"][&hub] = json!({});
                    }
                    if !v["hubs"][&hub].is_object() {
                        return Err("INVALID_INPUT: Hub must be an object".into());
                    }
                    v["hubs"][&hub]["enabled"] = json!(enabled);
                    if let Some(m) = v["hubs"][&hub].as_object_mut() {
                        m.remove("disabled");
                    }
                    repair_default_hub(v);
                    Ok(())
                })?;
            }
            "setup_remove_hub" | "remove_hub" => {
                if hub == "local" {
                    return Err("INVALID_INPUT: Cannot remove the local hub".into());
                }
                self.mutate_config(|v| {
                    if let Some(h) = v["hubs"].as_object_mut() {
                        h.remove(&hub);
                    }
                    repair_default_hub(v);
                    Ok(())
                })?;
                let projection = self.purge_hub_state(&hub).and_then(|_| self.reload_slots());
                return Ok(committed_projection(json!({"ok":true}), projection));
            }
            "test_hub" => return self.slot(&hub)?.rpc("account_info", &json!({})),
            "paste_hub_invite" => {
                let mut invite =
                    if let Some(config) = action.get("config").filter(|_| precondition.is_some()) {
                        config.clone()
                    } else {
                        parse_hub_invite(string(action, "invite"))?
                    };
                if action["parse_only"] == true {
                    if let Some(port) = invite
                        .get("redirectPort")
                        .or_else(|| invite.get("oidcRedirectPort"))
                        .cloned()
                    {
                        invite["redirect_port"] = port;
                    }
                    self.emit(json!({"op":"hub_invite","source_op":"paste_hub_invite","request_id":action["request_id"],"config":invite}));
                    return Ok(json!({"ok":true}));
                }
                let raw = self.raw_config()?;
                self.reject_own_hub_invite(&invite)?;
                let base = normalize_slug(string(&invite, "name"));
                if !string(&invite, "name").is_empty() {
                    let mut name = base.clone();
                    let mut suffix = 2;
                    while name == "local"
                        || raw["hubs"]
                            .get(&name)
                            .is_some_and(|hub| hub["url"] != invite["url"])
                    {
                        let ending = format!("-{suffix}");
                        name = format!(
                            "{}{}",
                            base.chars()
                                .take(32 - ending.len())
                                .collect::<String>()
                                .trim_end_matches('-'),
                            ending
                        );
                        suffix += 1;
                    }
                    invite["name"] = json!(name);
                }
                invite["request_id"] = action["request_id"].clone();
                return self.action_with_precondition(&invite, precondition);
            }
            "generate_hub_invite" => {
                let mut invite = self.slot(&hub)?.config.clone();
                invite["name"] = json!(hub);
                return Ok(json!({"ok":true,"invite":invite}));
            }
            "send_signal" | "send_reply_from_human" => {
                let caller =
                    json!({"harness":"human","session_id":"default","cwd":"","title":"Human"});
                let mut send_args = action.clone();
                if let Some(args) = send_args.as_object_mut() {
                    args.remove("op");
                    args.remove("request_id");
                    if op == "send_reply_from_human" {
                        args.insert(
                            "to".into(),
                            action["to_listener_id"]
                                .as_str()
                                .or_else(|| action["to"].as_str())
                                .map_or(Value::Null, |v| json!(v)),
                        );
                        args.insert(
                            "message".into(),
                            action["message"]
                                .as_str()
                                .or_else(|| action["text"].as_str())
                                .map_or(Value::Null, |v| json!(v)),
                        );
                        args.remove("in_reply_to");
                        if let Some(source) = action
                            .get("in_reply_to")
                            .filter(|v| !v.is_null())
                            .or_else(|| action.get("signal_id").filter(|v| !v.is_null()))
                        {
                            args.insert("in_reply_to".into(), source.clone());
                        }
                        for key in ["to_listener_id", "text", "signal_id"] {
                            args.remove(key);
                        }
                    }
                }
                let result = validate_tool_input("cito_send", &send_args)
                    .and_then(|()| self.send_from(&hub, &send_args, Some(caller)));
                return result.map(|result| {
                    if op == "send_reply_from_human" {
                        // Keep the reply action's response shape while sharing the atomic Human RPC.
                        let unscoped = |key| string(&result, key).rsplit(':').next().unwrap_or("");
                        json!({"ok":true,"signal_id":unscoped("signal_id"),"seq":result["seq"],"target_display":unscoped("to"),"error":null,"maintenance_warning":result["maintenance_warning"]})
                    } else {
                        result
                    }
                });
            }
            "ack_signals" | "mark_activity_read" | "mark_all_activity_read" => {
                let bulk = op == "mark_all_activity_read" || action["ack_all"] == true;
                self.state.acknowledge_activity(
                    action,
                    &self.activity_filter,
                    &self.host_id,
                    bulk,
                )?;
                self.flush_acks();
            }
            "copy_activity_message" => {
                let row = self.state.activity_message(&hub, &action["signal_id"])?;
                let message = string(&row, "message");
                let characters: Vec<_> = message.chars().collect();
                let chunks: Vec<String> = if characters.is_empty() {
                    vec![String::new()]
                } else {
                    characters
                        .chunks(16_000)
                        .map(|part| part.iter().collect())
                        .collect()
                };
                for (index, chunk) in chunks.iter().enumerate() {
                    self.emit(json!({"op":"activity_message","request_id":action["request_id"],"part":index,"complete":index+1==chunks.len(),"message":chunk}));
                }
                return Ok(json!({"ok":true}));
            }
            "clear_activity_history" => {
                self.state
                    .clear_activity_history(action, &self.activity_filter, &self.host_id)?;
                self.flush_acks();
            }
            "listen_session" => {
                if let Some(title) = action.get("title") {
                    crate::core::registration::validate_title(
                        title
                            .as_str()
                            .ok_or("INVALID_INPUT: title must be a string")?,
                    )?;
                }
                if let Some(tag) = action.get("tag") {
                    if !tag
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty() && s.chars().count() <= 64)
                    {
                        return Err("INVALID_INPUT: Invalid tag".into());
                    }
                }
                let mut caller = action.clone();
                if caller["session_id"].is_null() {
                    caller["session_id"] = json!("default");
                }
                if caller["harness"] != "human" {
                    if !self
                        .plugins
                        .agent_plugins()
                        .iter()
                        .any(|p| p.id == string(&caller, "harness"))
                    {
                        return Err("UNSUPPORTED_HARNESS: Unknown harness".into());
                    }
                    let mut meta = json!({"expected_harness":caller["harness"],"expected_session_id":caller["session_id"],"cwd":caller["cwd"]});
                    if caller["endpoint_pid"].is_number() {
                        meta["endpoint_pid"] = caller["endpoint_pid"].clone();
                    }
                    caller = self
                        .caller(&meta, true)?
                        .ok_or("BINDING_REQUIRED: Session is not open")?;
                }
                if caller["harness"] == "human" && caller["session_id"] != "default" {
                    return Err("INVALID_INPUT: Human session must be default".into());
                }
                if caller["harness"] == "human" && string(&caller, "title").trim().is_empty() {
                    caller["title"] = json!("Human");
                }
                let about = action["about"]
                    .as_str()
                    .filter(|about| !about.trim().is_empty())
                    .unwrap_or_else(|| {
                        action["title"]
                            .as_str()
                            .unwrap_or(if caller["harness"] == "human" {
                                "Human"
                            } else {
                                "Session"
                            })
                    });
                let mut args = json!({"about":about,"scope":action["scope"]});
                if let Some(tag) = action.get("tag") {
                    args["tag"] = tag.clone();
                }
                if caller["harness"] != "human" {
                    if let Some(title) = action.get("title") {
                        args["title"] = title.clone();
                    }
                }
                return self.listen(&hub, &args, &caller);
            }
            "unlisten" => {
                if !action["hub"].is_null() {
                    self.validate_configured_hub(&hub)?;
                }
                let id = string(action, "listener_id");
                if let Some(row) = self
                    .state
                    .listener(&hub, id)?
                    .filter(|r| r["harness"] != "human")
                {
                    self.ensure_agent_stop_row(
                        &hub,
                        string(&row, "harness"),
                        string(&row, "session_id"),
                    )?;
                } else if action["harness"].as_str().is_some_and(|h| h != "human") {
                    self.ensure_agent_stop_row(
                        &hub,
                        string(action, "harness"),
                        string(action, "session_id"),
                    )?;
                }
                self.unlisten_id(&hub, id)?;
            }
            "unlisten_all" => {
                if !action["hub"].is_null() {
                    self.validate_configured_hub(&hub)?;
                }
                // Bulk stop owns Hub policy even when no listener is active.
                let raw = self.raw_config()?;
                let (_, configured, _) =
                    crate::plugins::try_normalize_existing_config(Some(&raw.to_string()))?;
                let selected_hub = action.get("hub").filter(|hub| !hub.is_null());
                let targets: Vec<String> = configured
                    .keys()
                    .filter(|name| selected_hub.is_none_or(|hub| *hub == name.as_str()))
                    .cloned()
                    .collect();
                if targets.is_empty() && action["hub"].is_string() {
                    return Err("INVALID_INPUT: Unknown hub".into());
                }
                let mut removed = 0;
                let mut detach_errors = Vec::new();
                for row in self.state.active()?.into_iter().filter(|row| {
                    row["harness"] != "human" && selected_hub.is_none_or(|hub| row["hub"] == *hub)
                }) {
                    match self.unlisten_id(string(&row, "hub"), string(&row, "listener_id")) {
                        Ok(true) => removed += 1,
                        Ok(false) => {}
                        Err(error) => {
                            detach_errors.push(format!("{}: {error}", string(&row, "hub")))
                        }
                    }
                }
                for name in targets {
                    self.state.db.execute("UPDATE local_listeners SET user_stopped=1,retirement_reason=NULL WHERE hub=? AND harness!='human' AND status!='FENCED_INACTIVE'", &[json!(name)])?;
                    // Bulk stop reports incomplete detaches across all targets.
                    if let Err(error) = self
                        .set_auto_listen(&name, false)
                        .and_then(|projection| projection)
                    {
                        detach_errors.push(format!("{name}: {error}"));
                    }
                }
                if !detach_errors.is_empty() {
                    return Err(format!("DETACH_PENDING: {}", detach_errors.join("; ")));
                }
                return Ok(json!({"ok":true,"removed":removed}));
            }
            "set_listen_mode" => {
                let mode = string(action, "listen_mode");
                if !matches!(mode, "auto" | "manual") {
                    return Err("INVALID_INPUT: listen_mode must be auto or manual".into());
                }
                let slot = self.slot(&hub)?;
                if mode == "auto" && slot.config["autoListen"] != true {
                    return Err(
                        "INVALID_INPUT: Enable Auto-Listen before unpinning a session".into(),
                    );
                }
                let lock = self.state.registration_lock(&hub)?;
                let _guard = lock
                    .lock()
                    .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
                let row = self
                    .state
                    .listener(&hub, string(action, "listener_id"))?
                    .filter(|r| r["harness"] != "human" && r["status"] == "ACTIVE")
                    .ok_or("LISTENER_NOT_FOUND: Session is not active")?;
                let activity = self
                    .session_activity
                    .as_ref()
                    .into_iter()
                    .flat_map(|a| a.candidates())
                    .find(|c| {
                        c["harness"] == row["harness"] && c["session_id"] == row["session_id"]
                    })
                    .and_then(|c| super::super::state::qualifying_activity(&c));
                let at = activity.map(|(_, at)| at).unwrap_or_else(now_ms);
                self.state.db.execute("UPDATE local_listeners SET listen_mode=?,user_stopped=0,retirement_reason=NULL,retired_activity_revision=NULL,last_activity_at_ms=CASE WHEN listen_mode='manual' AND ?='auto' THEN MAX(COALESCE(last_activity_at_ms,0),?) ELSE last_activity_at_ms END WHERE hub=? AND listener_id=?", &[json!(mode),json!(mode),json!(at),json!(hub),row["listener_id"].clone()])?;
            }
            "set_scope" => {
                let id = string(action, "listener_id");
                let scope = string(action, "scope");
                if !matches!(scope, "self" | "workspace") {
                    return Err("INVALID_INPUT: Invalid scope".into());
                }
                let slot = self.slot(&hub)?;
                let registration = self.state.registration_lock(&hub)?;
                let _registration = registration
                    .lock()
                    .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
                for attempt in 0..3 {
                    let (mut row, rev) = slot.kv_get(id)?.ok_or("LISTENER_NOT_FOUND")?;
                    if !row.is_object() {
                        return Err("INVALID_INPUT: Listener must be an object".into());
                    }
                    if row["harness"] == "human" {
                        let result = slot.human_request("human_inbox_update",&json!({"node_id":id,"expected_control_revision":row["human_control_revision"],"scope":scope}))?;
                        let projection = Self::reconcile_human_scope(
                            &self.state,
                            slot,
                            &self.host_id,
                            &result["record"],
                        );
                        return Ok(committed_projection(result, projection));
                    }
                    row["scope"] = json!(scope);
                    match slot.kv_save(id, &row, Some(rev)) {
                        Ok(_) => break,
                        Err(error) if attempt == 2 || !error.starts_with("10071:") => {
                            return Err(error)
                        }
                        Err(_) => {}
                    }
                }
                let projection = self
                    .state
                    .db
                    .execute(
                        "UPDATE local_listeners SET scope=? WHERE hub=? AND listener_id=?",
                        &[json!(scope), json!(hub), json!(id)],
                    )
                    .map(|_| ());
                return Ok(committed_projection(json!({"ok":true}), projection));
            }
            "open_directory" | "query_directory" => {
                self.directory_open = true;
                if op == "open_directory" {
                    self.refresh_available()?;
                }
                self.activity_filter = action["filter"].as_str().unwrap_or("all").into();
                self.activity_query = if action["mode"] == "activity" {
                    string(action, "query").into()
                } else {
                    String::new()
                };
                let mut args = json!({"query":action["query"].as_str().unwrap_or(""),"limit":50});
                if let Some(hub) = action["hub"].as_str().filter(|s| !s.is_empty()) {
                    args["hub"] = json!(hub);
                }
                validate_tool_input("cito_find", &args)?;
                self.slot(&hub)?;
                let harness = action["harness"]
                    .as_str()
                    .filter(|id| !id.is_empty() && *id != "all");
                let mine = matches!(action["scope"].as_str(), Some("mine" | "self"));
                let result = self.find_listeners(&args, |row| {
                    harness.is_none_or(|id| row["harness"] == id)
                        && (!mine || row["is_self_principal"] == true)
                })?;
                let (activity, total, unread) = self.state.query_activity(
                    action["filter"].as_str().unwrap_or("all"),
                    if action["mode"] == "activity" {
                        string(action, "query")
                    } else {
                        ""
                    },
                )?;
                let (all_total, all_unread) = if action["filter"].as_str().unwrap_or("all") == "all"
                    && (action["mode"] != "activity" || string(action, "query").trim().is_empty())
                {
                    (total, unread)
                } else {
                    let (_, total, unread) = self.state.query_activity("all", "")?;
                    (total, unread)
                };
                let activity = self.activity_dto(activity, &self.address_book());
                let warnings: Vec<String> = result["warnings"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|warning| {
                        format!("{}: {}", string(warning, "hub"), string(warning, "message"))
                    })
                    .collect();
                let frame = json!({"op":"directory","hub":hub,"request_id":action["request_id"],"query":action["query"].as_str().unwrap_or(""),"scope":action["scope"].as_str().unwrap_or("all"),"harness":action["harness"].as_str().unwrap_or("all"),"matched_count":result["matched_count"],"online_count":result["online_count"],"peers":result["listeners"],"warnings":warnings,"activity":activity,"activity_total":total,"activity_all_total":all_total,"activity_has_any":all_total>0,"activity_unread_count":unread,"activity_filter":self.activity_filter,"activity_query":self.activity_query,"unread_count":all_unread});
                self.emit(frame.clone());
                return Ok(frame);
            }
            "switch_hub" => {
                let from = action["from_hub"].as_str().unwrap_or(&hub);
                let to = action["to_hub"]
                    .as_str()
                    .ok_or("INVALID_INPUT: to_hub required")?;
                self.slot(to)?;
                let target = string(action, "listener_id");
                let row = self
                    .state
                    .active()?
                    .into_iter()
                    .find(|r| {
                        string(r, "hub") == from
                            && (string(r, "listener_id") == target
                                || string(r, "public_id") == target)
                    })
                    .ok_or("LISTENER_NOT_FOUND: Session is not active on source Hub")?;
                if from != to {
                    let destination = self.slot(to)?;
                    let existing = crate::core::registration::local_node(
                        &self.state,
                        destination,
                        string(&row, "harness"),
                        string(&row, "session_id"),
                    )?;
                    let mut attached = json!({"ok":true});
                    if !existing.is_some_and(|value| value["status"] == "ACTIVE") {
                        let scope = if destination.config["authMode"] == "oidc"
                            && self.slot(from)?.config["authMode"] != "oidc"
                        {
                            json!("self")
                        } else {
                            row["scope"].clone()
                        };
                        let mut request = json!({"about":row["about"],"scope":scope});
                        if row["tag"].is_string() {
                            request["tag"] = row["tag"].clone();
                        }
                        attached = self.listen(to, &request, &row)?;
                    }
                    return Ok(committed_projection(
                        attached,
                        self.unlisten_id(from, string(&row, "listener_id"))
                            .map(|_| ()),
                    ));
                }
            }
            "set_host_hub" => {
                let mut server = self.host_server.take();
                let previous = server
                    .as_ref()
                    .map(|server| (server.address.ip(), server.port(), server.auth.clone()));
                let mut applied = false;
                let result = self.mutate_config(|v| {
                    let mut host = v
                        .get("host_hub")
                        .or_else(|| v.get("hostHub"))
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    if host.is_null() {
                        host = json!({});
                    }
                    if !host.is_object() {
                        return Err("INVALID_INPUT: Host hub must be an object".into());
                    }
                    for key in ["enabled", "port", "token", "auth_mode", "bind"] {
                        if let Some(value) = action.get(key) {
                            host[key] = value.clone();
                        }
                    }
                    if (host["enabled"] == true || host_hub_auth_mode(&host) == "token")
                        && host
                            .get("token")
                            .is_none_or(|token| token.as_str() == Some(""))
                    {
                        host["token"] = json!(crate::plugins::try_random_hex(32)?);
                    }
                    let settings = host_hub_server_config(&host)?;
                    if !self.closing {
                        replace_host_hub_server(&self.local, &mut server, settings)?;
                        applied = true;
                    }
                    v["host_hub"] = host;
                    if let Some(m) = v.as_object_mut() {
                        m.remove("hostHub");
                    }
                    Ok(())
                });
                // A failed config write must also undo a successful listener change.
                let rollback = if result.is_err() && applied {
                    replace_host_hub_server(&self.local, &mut server, previous)
                } else {
                    Ok(())
                };
                self.host_server = server;
                if let Err(error) = result {
                    let error = match rollback {
                        Ok(()) => error,
                        Err(rollback) => {
                            format!("{error}; failed to restore host hub listener: {rollback}")
                        }
                    };
                    self.host_hub_error = Some(super::HostHubError::Action(error.clone()));
                    return Err(error);
                }
                self.applied_host_config =
                    self.raw_config().ok().map(|raw| raw["host_hub"].clone());
                self.host_hub_error = None;
                self.emit(json!({"op":"settings","ok":true,"committed":true,"source_op":action["op"],"request_id":action["request_id"],"settings":self.settings()}));
            }
            "set_launch_at_login" => {
                set_launch_at_login(
                    action["enabled"].as_bool().unwrap_or(false),
                    &self.config.state_dir,
                    &self.config.home_dir,
                    &self.plugins,
                )?;
            }
            "setup_disconnect_all" => {
                for row in self
                    .state
                    .active()?
                    .into_iter()
                    .filter(|row| action["purge_data"] == true || row["harness"] != "human")
                {
                    self.retire_id(
                        string(&row, "hub"),
                        string(&row, "listener_id"),
                        "disconnected",
                    )?;
                }
                for p in self
                    .plugins
                    .agent_plugins()
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>()
                {
                    remove_plugin_setup(&p)?;
                    self.invalidate_setup(&p.id);
                    self.last_app_action
                        .insert(p.id.clone(), "disconnect".into());
                }
                self.caller_cache
                    .lock()
                    .map_err(|_| "Caller cache lock poisoned")?
                    .clear();
                if action["purge_data"] == true {
                    set_launch_at_login(
                        false,
                        &self.config.state_dir,
                        &self.config.home_dir,
                        &self.plugins,
                    )?;
                    let principal =
                        crate::core::auth::local_principal(&self.config.effective_user("local")?);
                    let inventory =
                        self.local
                            .rpc("human_channels_list", &json!({}), Some(&principal))?;
                    for channel in inventory["channels"].as_array().into_iter().flatten() {
                        self.local.rpc(
                            "human_channels_configure",
                            &json!({"plugin_id":channel["id"],"reset_hub":true,"reset_user":true}),
                            Some(&principal),
                        )?;
                    }
                    self.mutate_config(|v| {
                        v["default_hub"] = json!("local");
                        if let Some(object) = v.as_object_mut() {
                            object.remove("defaultHub");
                        }
                        v["hubs"] = json!({});
                        v["host_hub"] = json!({"enabled":false});
                        if let Some(object) = v.as_object_mut() {
                            object.remove("hostHub");
                        }
                        Ok(())
                    })?;
                    let logins = self
                        .logins
                        .drain()
                        .map(|(_, attempt)| attempt)
                        .collect::<Vec<_>>();
                    for attempt in &logins {
                        attempt.cancelled.store(true, Ordering::SeqCst);
                    }
                    for attempt in &logins {
                        drop(attempt.serial.lock().map_err(|_| "Login lock poisoned")?);
                    }
                    for slot in self.slots.values() {
                        slot.close();
                    }
                    for (_, worker) in self.receivers.drain() {
                        let _ = worker.join();
                    }
                    for worker in self.events.drain(..) {
                        let _ = worker.join();
                    }
                    self.slots.clear();
                    self.channel_inventories.clear();
                    self.active_settings_hub = None;
                    if let Some(server) = self.host_server.take() {
                        server.shutdown();
                    }
                    if let Some(mut activity) = self.session_activity.take() {
                        activity.shutdown()?;
                    }
                    for pending in self
                        .oidc_recoveries
                        .values()
                        .chain(self.retired_auth.iter())
                    {
                        pending.cancelled.store(true, Ordering::SeqCst);
                    }
                    for pending in self
                        .oidc_recoveries
                        .drain()
                        .map(|(_, p)| p)
                        .chain(self.retired_auth.drain(..))
                    {
                        let _ = pending.worker.join();
                    }
                    for entry in fs::read_dir(&self.config.state_dir).map_err(|e| e.to_string())? {
                        let entry = entry.map_err(|e| e.to_string())?;
                        if entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with("oidc-token-")
                        {
                            fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
                        }
                    }
                    self.local.reset_data()?;
                    match fs::remove_dir_all(self.config.state_dir.join("activity")) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => {
                            return Err(
                                "STATE_ERROR: Cannot remove session activity metadata".into()
                            )
                        }
                    }
                    self.state.db.transaction(|db| {
                        db.execute("DELETE FROM local_sent_idempotency; DELETE FROM pending_hub_acks; DELETE FROM pending_hub_detaches; DELETE FROM wake_retries;", &[])?;
                        db.execute("DELETE FROM local_signals", &[])?;
                        db.execute("DELETE FROM local_listeners", &[])?;
                        db.execute("DELETE FROM session_custom_titles", &[])?;
                        Ok(())
                    })?;
                    self.session_activity =
                        Some(crate::core::session_activity::SessionActivity::new(
                            &self.config,
                            &self.plugins,
                        ));
                }
                if let Some(activity) = self.session_activity.as_mut() {
                    for plugin in self.plugins.agent_plugins() {
                        activity.remove_candidates(&plugin.id);
                    }
                }
            }
            "setup_uninstall_all" => {
                for row in self.state.active()? {
                    let _ = self.retire_id(
                        string(&row, "hub"),
                        string(&row, "listener_id"),
                        "disconnected",
                    );
                }
                self.uninstall = Some(action["purge_data"].as_bool().unwrap_or(false));
                request_shutdown();
            }
            "quit" => request_shutdown(),
            "set_auto_listen" => {
                if !self.slots.contains_key(&hub) {
                    return Err("INVALID_INPUT: Enabled hub required".into());
                }
                let enabled = action["enabled"]
                    .as_bool()
                    .ok_or("INVALID_INPUT: enabled must be a boolean")?;
                let projection = self
                    .set_auto_listen(&hub, enabled)?
                    .and_then(|_| if enabled { self.auto_listen() } else { Ok(()) })
                    .and_then(|_| self.reload_slots());
                return Ok(committed_projection(json!({"ok":true}), projection));
            }
            "refresh_available" => {
                self.refresh_available()?;
                self.poll()?;
            }
            "close_settings" => {
                self.active_settings_hub = None;
            }
            "close_directory" => {
                self.directory_open = false;
            }
            _ => return Err(format!("INVALID_INPUT: Unknown UI action: {op}")),
        }
        let reload = self.reload_slots();
        if mutating_action(action) {
            return Ok(committed_projection(json!({"ok":true}), reload));
        }
        reload?;
        // Setup receipts require the refreshed inspection, which dispatch_action
        // awaits without holding service. Passive cached projections remain usable.
        if !matches!(op, "close_menu" | "close_settings" | "close_directory") {
            self.push_ui_result(Some(action));
        }
        Ok(json!({"ok":true}))
    }
}
