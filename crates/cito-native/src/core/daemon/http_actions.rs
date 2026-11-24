use super::{
    channels::{channel_action_kind, dispatch_channel_action},
    host_hub::{
        host_hub_server_config, hub_name_from_url, parse_hub_invite, replace_host_hub_server,
        validate_hub_endpoints,
    },
    mcp::{run_captured, validate_tool_input, Detaches, ToolCall},
    request_shutdown, shutdown_requested,
    ui_ipc::bounded_frame,
    wake_daemon, ConfigSnapshot, DaemonService, SHUTDOWN_REQUESTED,
};
use crate::core::{
    config::{apply_host_hub_patch, repair_default_hub},
    identity::{listener_id, normalize_slug, string},
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

const HUB_TABLES: &[&str] = &[
    "local_signals",
    "local_listeners",
    "pending_hub_acks",
    "pending_hub_detaches",
    "local_sent_idempotency",
    "wake_retries",
];

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

fn available_hub_name(base: String, hubs: &Value, url: &Value, original: Option<&str>) -> String {
    let mut name = base.clone();
    let mut suffix = 2;
    while name == "local"
        || (Some(name.as_str()) != original
            && hubs
                .get(&name)
                .is_some_and(|existing| existing["url"] != *url))
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
    name
}

fn edited_hub(action: &Value) -> Option<String> {
    if action["edit"] != true {
        return None;
    }
    action["original_name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            let name = string(action, "name");
            name.bytes()
                .any(|b| b.is_ascii_alphanumeric())
                .then(|| normalize_slug(name))
        })
}

fn invite_hub_name(invite: &Value, raw: &Value) -> String {
    let base = if string(invite, "name").is_empty() {
        hub_name_from_url(string(invite, "url"))
    } else {
        normalize_slug(string(invite, "name"))
    };
    available_hub_name(base, &raw["hubs"], &invite["url"], None)
}

fn configured_hubs(snapshot: &ConfigSnapshot) -> Result<Map<String, Value>, String> {
    snapshot.normalized().map(|(_, hubs, _)| hubs)
}

fn set_profile_enabled(
    raw: &mut Value,
    name: &str,
    enabled: bool,
    address: Option<std::net::SocketAddr>,
) -> Result<(), String> {
    let hub = raw["hubs"][name]
        .as_object_mut()
        .ok_or("INVALID_INPUT: Hub must be an object")?;
    hub.insert("enabled".into(), json!(enabled));
    hub.remove("disabled");
    repair_default_hub(raw);
    validate_hub_endpoints(raw, address, Some(name))
}

fn action_hub(action: &Value) -> Result<&str, String> {
    action["hub"]
        .as_str()
        .filter(|hub| !hub.trim().is_empty())
        .ok_or_else(|| "INVALID_INPUT: hub must be a non-empty string".into())
}

pub(super) fn interactive_login(action: &Value) -> bool {
    action["op"] == "hub_action"
        && matches!(action["action"].as_str(), Some("login" | "switch_account"))
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

pub(super) fn check_setup_actions(actions: Vec<crate::setup::SetupAction>) -> Result<(), String> {
    if let Some(action) = actions.into_iter().find(|a| a.status == "conflict") {
        return Err(format!(
            "SETUP_CONFLICT: {}: {}",
            action.path, action.detail
        ));
    }
    Ok(())
}

fn refreshes_setup(action: &Value) -> bool {
    matches!(
        string(action, "op"),
        "setup_apply" | "setup_remove_app" | "setup_disconnect_all"
    )
}

/// Wait without holding the service lock. The returned guard pins the inspected
/// generation while the caller selects discovery providers or publishes a receipt.
fn settle_setup<'a, S: std::borrow::BorrowMut<DaemonService>>(
    owner: &'a Mutex<S>,
    mut guard: std::sync::MutexGuard<'a, S>,
    ui: Option<u64>,
) -> Result<(std::sync::MutexGuard<'a, S>, Result<(), String>), String> {
    let completion = (*guard).borrow_mut().setup_completion.clone();
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let before = *completion.0.lock().unwrap_or_else(|e| e.into_inner());
        let s = (*guard).borrow_mut();
        if let Err(error) = s.check_action(ui) {
            return Ok((guard, Err(error)));
        }
        s.refresh_setup_inspections();
        if s.setup_inspections.values().all(|e| e.worker.is_none()) {
            return Ok((guard, Ok(())));
        }
        if Instant::now() >= deadline {
            return Ok((guard, Err("Setup inspection timed out".into())));
        }
        drop(guard);
        let revision = completion.0.lock().unwrap_or_else(|e| e.into_inner());
        drop(
            completion
                .1
                .wait_timeout_while(
                    revision,
                    deadline.saturating_duration_since(Instant::now()),
                    |v| *v == before,
                )
                .unwrap_or_else(|e| e.into_inner()),
        );
        guard = owner.lock().map_err(|_| "Daemon lock poisoned")?;
    }
}

pub(super) fn dispatch_action(
    service: &Arc<Mutex<DaemonService>>,
    action: &Value,
    ui_generation: Option<u64>,
) -> Result<Value, String> {
    let serial = if matches!(
        string(action, "op"),
        "setup_apply" | "setup_disconnect_all" | "setup_remove_app" | "setup_uninstall_all"
    ) {
        Some(
            service
                .lock()
                .map_err(|_| "Daemon lock poisoned")?
                .setup_serial
                .clone(),
        )
    } else {
        None
    };
    let _setup = serial
        .as_ref()
        .map(|lock| lock.lock().map_err(|_| "Setup lock poisoned"))
        .transpose()?;
    let mut result = dispatch_action_inner(service, action, ui_generation);
    drop(_setup);
    let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
    if refreshes_setup(action) {
        let (guard, inspection) = settle_setup(service, s, ui_generation)?;
        s = guard;
        if let Err(error) = s.validate_ui_generation(ui_generation) {
            return result.map(|value| committed_projection(value, Err(error)));
        }
        if let Err(error) = inspection {
            result = result.map(|value| committed_projection(value, Err(error)));
        }
    } else if action["op"] != "send_signal" {
        // Composer actions settle under their original guard.
        if let Err(error) = s.validate_ui_generation(ui_generation) {
            return match result {
                Ok(value) if value["committed"] == true => {
                    Ok(committed_projection(value, Err(error)))
                }
                _ => Err(error),
            };
        }
    }
    if interactive_login(action) {
        s.record_hub_result(action, &result, None);
    }
    if let Err(error) = &result {
        if channel_action_kind(action) != Some(super::channels::ChannelActionKind::Mutation)
            && !matches!(string(action, "op"), "set_host_hub" | "send_signal")
        {
            s.emit(
            json!({"op":"error","error":error,"source_op":action["op"],"request_id":action["request_id"],"hub":action["hub"]}),
        );
        }
    }
    if refreshes_setup(action) && result.is_ok() {
        s.emit_settings_receipt(action, &result);
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
    if action["op"] == "setup_apply" {
        let id = action["harness"]
            .as_str()
            .ok_or("INVALID_INPUT: harness required")?;
        let (descriptor, retired, inspections) = {
            let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
            s.validate_ui_generation(ui_generation)?;
            if s.closing || shutdown_requested() {
                return Err("Shutting down".into());
            }
            let p = s
                .plugins
                .find_by_id(id)
                .ok_or("UNSUPPORTED_HARNESS: Unknown harness")?;
            ensure_single_plugin_staged(&s.config.state_dir, p)?;
            let plugins = crate::plugins::load_plugins(&s.config.state_dir);
            let changed: std::collections::HashSet<_> = plugins
                .agent_plugins()
                .into_iter()
                .filter(|p| {
                    s.plugins.find_by_id(&p.id).is_none_or(|old| {
                        old.command != p.command
                            || old.plugin_dir != p.plugin_dir
                            || old.kind != p.kind
                            || old.is_builtin != p.is_builtin
                            || old.auto_listen.recent_window_ms != p.auto_listen.recent_window_ms
                            || old.display_name != p.display_name
                    })
                })
                .map(|p| p.id.clone())
                .collect();
            let config = s.config.clone();
            let retired = s
                .session_activity
                .as_mut()
                .map(|a| a.reload(&config, &plugins, &changed));
            let mut inspections = Vec::new();
            s.setup_inspections.retain(|id, entry| {
                let keep =
                    !changed.contains(id) && plugins.agent_plugins().iter().any(|p| &p.id == id);
                if !keep {
                    if let Some((_, worker)) = entry.worker.take() {
                        inspections.push(worker);
                    }
                }
                keep
            });
            s.caller_cache
                .lock()
                .map_err(|_| "Caller cache lock poisoned")?
                .retain(|_, v| {
                    plugins.find_by_id(string(v, "harness")).is_some()
                        && !changed.contains(string(v, "harness"))
                });
            s.plugins = plugins;
            // A replacement descriptor needs its own initial inspection. Snapshots may
            // reuse cached settings only while refreshing the same descriptor.
            for id in changed {
                s.setup_inspections.entry(id).or_default();
            }
            s.refresh_setup_inspections();
            (s.plugins.find_by_id(id).cloned(), retired, inspections)
        };
        drop(retired);
        for worker in inspections {
            let _ = worker.join();
        }
        let descriptor = descriptor.ok_or("UNSUPPORTED_HARNESS: Staged harness unavailable")?;
        let result = apply_plugin_setup(&descriptor, true, false);
        let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
        s.invalidate_setup(id);
        check_setup_actions(result?)?;
        return Ok(committed_projection(
            json!({"ok":true,"committed":true}),
            s.refresh_available(),
        ));
    }
    if let Some(kind) = channel_action_kind(action) {
        return dispatch_channel_action(service, action, kind, ui_generation);
    }
    if !interactive_login(action) {
        return execute_action(service, action, ui_generation);
    }
    let (hub, config, state_dir, attempt, snapshot) = {
        let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
        s.check_action(ui_generation)?;
        let hub = action_hub(action)?.to_owned();
        s.reload_slots()?;
        s.mutate_config(|v| {
            if !v["hubs"].is_object() || !v["hubs"][&hub].is_object() {
                return Err("INVALID_INPUT: Unknown hub".into());
            }
            set_profile_enabled(
                v,
                &hub,
                true,
                s.host_server.as_ref().map(|server| server.address),
            )
        })?;
        s.reload_slots()?;
        let config = s.slot(&hub)?.config.clone();
        let snapshot =
            login_cache_snapshot(&s.config.state_dir.join(format!("oidc-token-{hub}.json")));
        let attempt = s
            .hub_controls
            .entry(hub.clone())
            .or_default()
            .login
            .get_or_insert_with(Default::default)
            .clone();
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
        crate::core::login::recover_cancellable(
            &state_dir,
            &hub,
            &config,
            rejected,
            true,
            action["action"] == "switch_account",
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
    let current = s.hub_controls.get_mut(&hub).is_some_and(|control| {
        let current = control
            .login
            .as_ref()
            .is_some_and(|login| Arc::ptr_eq(login, &attempt));
        if current && Arc::strong_count(&attempt) == 2 {
            control.login = None;
        }
        current
    });
    if !current || attempt.cancelled.load(Ordering::SeqCst) || s.closing || shutdown_requested() {
        return Err("OIDC_LOGIN_TIMEOUT: OIDC login cancelled".into());
    }
    s.reload_slots()?;
    if !s.slots.get(&hub).is_some_and(|slot| slot.config == config) {
        return Err("OIDC_LOGIN_TIMEOUT: Hub changed during login".into());
    }
    let (token, identity) = result?;
    if let Some(old) = s.slots.remove(&hub) {
        old.close_and_drain()?;
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

/// Capture the work under service, release it for discovery, registration locks and RPCs,
/// then settle against the captured Hub and UI generations.
fn execute_action<S: std::borrow::BorrowMut<DaemonService>>(
    owner: &Mutex<S>,
    action: &Value,
    ui: Option<u64>,
) -> Result<Value, String> {
    let mut guard = owner.lock().map_err(|_| "Daemon lock poisoned")?;
    let s = (*guard).borrow_mut();
    let op = string(action, "op");
    let send = op == "send_signal";
    if let Err(error) = s.check_action(ui) {
        let result = Err(error);
        if send {
            s.emit_composer_result(action, &result);
        }
        return result;
    }
    if op == "setup_save_hub" || (op == "paste_hub_invite" && action["parse_only"] != true) {
        let before = s.raw_config()?;
        let mut resolved = None;
        let mut submitted = if op == "paste_hub_invite" {
            let mut parsed = parse_hub_invite(string(action, "invite"))?;
            parsed["request_id"] = action["request_id"].clone();
            parsed
        } else {
            action.clone()
        };
        let patch = if op == "setup_save_hub" {
            submitted
                .get("config")
                .filter(|v| v.is_object())
                .unwrap_or(&submitted)
        } else {
            &submitted
        };
        let base = edited_hub(&submitted)
            .and_then(|original| before["hubs"].get(original))
            .unwrap_or(&Value::Null);
        let merged = crate::core::config::merge_hub_config(base, patch);
        // Refuse a self-invite before contacting its discovery endpoint. The
        // locked save rechecks the entire profile with the then-current port.
        let own = merged["enabled"] != false
            && merged["disabled"] != true
            && super::host_hub::is_own_host_hub_url(
                string(&merged, "url"),
                s.host_server.as_ref().map(|server| server.address),
            );
        drop(guard);
        let discovered = if own {
            Err(super::host_hub::OWN_HUB_ERROR.into())
        } else {
            crate::core::config::discover_hub_config(merged)
        };
        guard = owner.lock().map_err(|_| "Daemon lock poisoned")?;
        let s = (*guard).borrow_mut();
        s.check_action(ui)?;
        if op == "paste_hub_invite" {
            // Discovery may normalize the URL. Resolve collisions against the same
            // config snapshot that the save will check under its publication lock.
            resolved = Some(invite_hub_name(
                discovered.as_ref().unwrap_or(&submitted),
                &before,
            ));
            submitted["name"] = json!(resolved);
        }
        let result = discovered.and_then(|config| {
            submitted["config"] = config;
            s.reload_slots()?;
            s.save_hub(&submitted, &mut resolved, &before)?;
            Ok(committed_projection(json!({"ok":true}), s.reload_slots()))
        });
        return DaemonService::finish_action(
            owner,
            guard,
            action,
            &submitted,
            resolved.as_deref(),
            None,
            result,
        );
    }
    if op == "refresh_available" {
        for entry in s.setup_inspections.values_mut() {
            entry.invalidate();
        }
        let (current, inspected) = settle_setup(owner, guard, ui)?;
        inspected?;
        guard = current;
    }
    let s = (*guard).borrow_mut();
    // Compare the actual file across retirement even if its JSON is damaged.
    let lifecycle = matches!(
        op,
        "setup_disconnect_all" | "setup_uninstall_all" | "unlisten_all"
    )
    .then(|| s.config_snapshot())
    .transpose()?;
    let recover = (matches!(op, "setup_disconnect_all" | "setup_uninstall_all")
        || (op == "unlisten_all" && action["hub"].is_null()))
        && lifecycle
            .as_ref()
            .is_some_and(|snapshot| configured_hubs(snapshot).is_err());
    let result = match s.prepare_action(action, ui, recover) {
        Ok(ToolCall::Ready(value)) => Ok(value),
        Err(error) => Err(error),
        Ok(work) => {
            let access = s.access.clone();
            let request = s.directory_request;
            let hub = action["hub"].as_str().unwrap_or(&access.config.default_hub);
            drop(guard);
            // UI sends publish exactly once; MCP's recovery and poll are specific to tool calls.
            let result = if send || matches!(work, ToolCall::HubControl { .. }) {
                work.execute(&access)
            } else {
                run_captured(owner, access.clone(), &work)
            };
            guard = owner.lock().map_err(|_| "Daemon lock poisoned")?;
            let s = (*guard).borrow_mut();
            let ui_current = s.check_action(ui).is_ok();
            let stale = !ui_current || !work.current(s, &access);
            let result = 'settle: {
                if let Some(before) = lifecycle
                    .as_ref()
                    .filter(|_| op != "unlisten_all" || recover)
                {
                    if stale
                        || s.config_snapshot()? != *before
                        || s.slots.len() != access.slots.len()
                        || access.slots.iter().any(|(hub, old)| {
                            s.slots
                                .get(hub)
                                .is_none_or(|live| !Arc::ptr_eq(&live.closed, &old.closed))
                        })
                    {
                        break 'settle Err("STALE_GENERATION: Listener retirement started; setup operation was not completed".into());
                    }
                }
                if matches!(work, ToolCall::Find { .. }) {
                    if stale || !s.directory_open || request != s.directory_request {
                        break 'settle Err("STALE_GENERATION: Directory query superseded".into());
                    }
                    result.and_then(|value| s.directory_result(action, hub, &value))
                } else if stale {
                    work.degraded(result, super::mcp::STALE_HUB)
                } else {
                    result
                }
            };
            if !ui_current {
                return result;
            }
            result
        }
    };
    DaemonService::finish_action(
        owner,
        guard,
        action,
        action,
        None,
        lifecycle.as_ref(),
        result,
    )
}

pub(super) fn mutating_action(action: &Value) -> bool {
    (action["op"] == "paste_hub_invite" && action["parse_only"] != true)
        || matches!(
            string(action, "op"),
            "setup_save_hub"
                | "set_host_hub"
                | "setup_apply"
                | "setup_remove_app"
                | "setup_disconnect_all"
                | "setup_remove_hub"
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
                | "rename_session"
                | "cli_rename"
                | "cli_human_device"
                | "unlisten_session"
                | "unlisten_all"
                | "ack_signals"
                | "clear_activity_history"
        )
}

fn human_send_args(action: &Value) -> Result<Value, String> {
    let mut args = action.clone();
    let fields = args
        .as_object_mut()
        .ok_or("INVALID_INPUT: Action must be an object")?;
    fields.remove("op");
    fields.remove("request_id");
    validate_tool_input("cito_send", &args)?;
    Ok(args)
}

impl super::HubAccess {
    pub(super) fn switch_binding(
        &self,
        from: &str,
        to: &str,
        row: &Value,
    ) -> Result<Value, String> {
        let destination = self.slot(to)?;
        let source = self.slot(from)?;
        let existing = crate::core::registration::local_node(
            &self.state,
            destination,
            string(row, "harness"),
            string(row, "session_id"),
        )?;
        let mut attached = json!({"ok":true});
        if !existing.is_some_and(|value| value["status"] == "ACTIVE") {
            let scope = if destination.config["authMode"] == "oidc"
                && source.config["authMode"] != "oidc"
            {
                json!("self")
            } else {
                row["scope"].clone()
            };
            let mut request = json!({"about":row["about"],"scope":scope});
            if row["tag"].is_string() {
                request["tag"] = row["tag"].clone();
            }
            attached = self.listen(to, &request, row)?;
        }
        let retire = if destination.closed.load(Ordering::Acquire) {
            Err("STALE_GENERATION: Destination replaced before source retirement".into())
        } else {
            self.retire_binding(
                from,
                string(row, "listener_id"),
                "user",
                Some(&source.closed),
                Some(row),
            )
            .map(|_| ())
        };
        Ok(committed_projection(attached, retire))
    }
}

pub(super) fn hub_control(
    slot: &crate::core::transport::HubClient,
    state: &crate::core::state::LocalState,
    host: &str,
    action: &Value,
) -> Result<Value, String> {
    let registration = state.registration_lock(&slot.name)?;
    let _guard = registration
        .lock()
        .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
    if slot.closed.load(Ordering::Acquire) {
        return Err("STALE_GENERATION: Hub authority replaced".into());
    }
    let human_update = |params: Value| {
        let result = slot.human_request("human_inbox_update", &params)?;
        let projection = DaemonService::reconcile_human_scope(state, slot, host, &result["record"]);
        Ok(committed_projection(result, projection))
    };
    if action["op"] == "human_inbox_update" {
        let id = slot
            .cached_directory()
            .into_iter()
            .find(|r| {
                r["harness"] == "human"
                    && crate::core::auth::principal_ids_equal(
                        string(r, "principal_id"),
                        string(&slot.identity, "principal_id"),
                    )
            })
            .map(|r| string(&r, "listener_id").to_owned())
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
                .filter(Value::is_number)
                .unwrap_or(json!(1));
        }
        return human_update(params);
    }
    let id = string(action, "listener_id");
    let scope = string(action, "scope");
    if !matches!(scope, "self" | "workspace") {
        return Err("INVALID_INPUT: Invalid scope".into());
    }
    for attempt in 0..3 {
        let (mut row, rev) = slot.kv_get(id)?.ok_or("LISTENER_NOT_FOUND")?;
        if !row.is_object() {
            return Err("INVALID_INPUT: Listener must be an object".into());
        }
        if row["harness"] == "human" {
            return human_update(
                json!({"node_id":id,"expected_control_revision":row["human_control_revision"],"scope":scope}),
            );
        }
        row["scope"] = json!(scope);
        match slot.kv_save(id, &row, Some(rev)) {
            Ok(_) => break,
            Err(error) if attempt == 2 || !error.starts_with("10071:") => return Err(error),
            Err(_) => {}
        }
    }
    let projection = state.db.transaction(|db| {
        if slot.closed.load(Ordering::Acquire) {
            return Err("STALE_GENERATION: Hub authority replaced".into());
        }
        db.execute(
            "UPDATE local_listeners SET scope=? WHERE hub=? AND listener_id=?",
            &[json!(scope), json!(slot.name), json!(id)],
        )?;
        Ok(())
    });
    Ok(committed_projection(json!({"ok":true}), projection))
}

impl DaemonService {
    fn check_action(&self, ui: Option<u64>) -> Result<(), String> {
        self.validate_ui_generation(ui)?;
        if self.purging {
            return Err("STATE_ERROR: Local data reset in progress".into());
        }
        if self.closing || shutdown_requested() {
            return Err("Shutting down".into());
        }
        Ok(())
    }

    fn validate_configured_hub(&self, hub: &str) -> Result<(), String> {
        let (_, hubs, _) =
            crate::plugins::try_normalize_existing_config(Some(&self.raw_config()?.to_string()))?;
        if hubs.get(hub).is_some_and(|cfg| cfg["enabled"] != false) {
            Ok(())
        } else {
            Err("INVALID_INPUT: Unknown or disabled hub".into())
        }
    }
    fn teardown_hub_runtime(&mut self, hub: &str) -> Result<(), String> {
        self.cancel_hub_interaction(hub);
        if let Some(slot) = self.slots.remove(hub) {
            slot.close_and_drain()?;
        }
        self.stop_hub_recovery(hub);
        let belongs = |key: &str| {
            key.starts_with(&format!("{hub}:"))
                || ["directory", "channels", "auto", "acks", "auto-agent"]
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
                // The closed slot fences SQLite writes. Keep ownership for
                // shutdown without waiting for registration or HTTP under service.
                self.events.push(worker);
            }
        }
        self.last_hub_errors.remove(hub);
        self.channel_inventories.remove(hub);
        self.observations.retain(|key, _| !belongs(key));
        if let Ok(mut retries) = self.ack_backoff.lock() {
            retries.retain(|key, _| {
                serde_json::from_str::<Value>(key).is_ok_and(|value| value[0] != hub)
            });
        }
        Ok(())
    }

    pub(super) fn purge_hub_state(&mut self, hub: &str) -> Result<(), String> {
        self.teardown_hub_runtime(hub)?;
        self.state.db.transaction(|db| {
            for table in HUB_TABLES {
                db.execute(&format!("DELETE FROM {table} WHERE hub=?"), &[json!(hub)])?;
            }
            Ok(())
        })?;
        let token = self.config.state_dir.join(format!("oidc-token-{hub}.json"));
        match fs::remove_file(token) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }?;
        self.hub_controls.remove(hub);
        Ok(())
    }

    pub fn action(&mut self, action: &Value) -> Result<Value, String> {
        execute_action(&Mutex::new(self), action, None)
    }

    fn save_hub(
        &mut self,
        action: &Value,
        resolved_hub: &mut Option<String>,
        before: &Value,
    ) -> Result<(), String> {
        let requested_name = string(action, "name").trim();
        if !requested_name.is_empty() && !requested_name.chars().any(|c| c.is_ascii_alphanumeric())
        {
            return Err("INVALID_INPUT: Invalid hub name".into());
        }
        let mut cfg = action["config"].clone();
        let editing = action["edit"] == true;
        let original = edited_hub(action).unwrap_or_default();
        if editing && original.is_empty() {
            return Err("INVALID_INPUT: Unknown hub to edit".into());
        }
        let mut rename_before = Value::Null;
        let mut renamed_config = Value::Null;
        self.mutate_config(|v| {
            if before != v {
                return Err("INVALID_INPUT: Configuration changed during discovery; retry".into());
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
            if editing && !v["hubs"][&original].is_object() {
                return Err("INVALID_INPUT: Unknown hub to edit".into());
            }
            let base_name = if requested_name.is_empty() {
                hub_name_from_url(string(&cfg, "url"))
            } else {
                normalize_slug(requested_name)
            };
            let name = if requested_name.is_empty() {
                available_hub_name(
                    base_name,
                    &v["hubs"],
                    &cfg["url"],
                    editing.then_some(original.as_str()),
                )
            } else {
                base_name
            };
            *resolved_hub = Some(name.clone());
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
                    return Err("INVALID_INPUT: Destination Hub credentials already exist".into());
                }
                if !v["hubs"][&name].is_null() {
                    return Err("INVALID_INPUT: Hub name already exists".into());
                }
                for table in HUB_TABLES {
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
                            "INVALID_INPUT: Destination Hub name has retained local data".into(),
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
            renamed_config = cfg.clone();
            v["hubs"][&name] = cfg;
            if set_default {
                v["default_hub"] = json!(name);
                if let Some(m) = v.as_object_mut() {
                    m.remove("defaultHub");
                }
            }
            repair_default_hub(v);
            validate_hub_endpoints(
                v,
                self.host_server.as_ref().map(|server| server.address),
                Some(&name),
            )
        })?;
        if let Some(name) = resolved_hub.as_deref() {
            self.cancel_hub_interaction(name);
        }
        if let Some(name) = resolved_hub
            .as_ref()
            .filter(|name| editing && **name != original)
        {
            self.teardown_hub_runtime(&original)?;
            let old_token = self
                .config
                .state_dir
                .join(format!("oidc-token-{original}.json"));
            let new_token = self
                .config
                .state_dir
                .join(format!("oidc-token-{name}.json"));
            let migration = self.state.db.transaction(|db| {
                for table in HUB_TABLES {
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
                    fs::rename(&new_token, &old_token).map_err(|recovery| {
                        format!(
                            "Hub rename failed: {error}; credential rollback failed: {recovery}"
                        )
                    })?;
                }
                self.mutate_config(|v| {
                    if v["hubs"][name] != renamed_config {
                        return Err(
                            "Hub rename rollback conflicts with a newer configuration".into()
                        );
                    }
                    if let Some(hubs) = v["hubs"].as_object_mut() {
                        hubs.remove(name);
                        if !rename_before["hubs"][&original].is_null() {
                            hubs.insert(original.clone(), rename_before["hubs"][&original].clone());
                        }
                    }
                    if v["default_hub"] == *name {
                        v["default_hub"] = rename_before
                            .get("default_hub")
                            .or_else(|| rename_before.get("defaultHub"))
                            .cloned()
                            .unwrap_or(json!("local"));
                    }
                    Ok(())
                })
                .map_err(|recovery| {
                    format!("Hub rename failed: {error}; configuration rollback failed: {recovery}")
                })?;
                return Err(error);
            }
            self.hub_controls.remove(&original);
        }
        Ok(())
    }
    fn finish_action<'a, S: std::borrow::BorrowMut<DaemonService>>(
        owner: &'a Mutex<S>,
        mut guard: std::sync::MutexGuard<'a, S>,
        action: &Value,
        operation: &Value,
        resolved_hub: Option<&str>,
        lifecycle: Option<&ConfigSnapshot>,
        result: Result<Value, String>,
    ) -> Result<Value, String> {
        let ingress_ui = (*guard).borrow_mut().ui_generation;
        let mut result = result;
        if let Some(before) =
            lifecycle.filter(|_| operation["op"] == "setup_disconnect_all" && result.is_ok())
        {
            let ready = |s: &DaemonService| -> Result<(), String> {
                s.check_action(Some(ingress_ui))?;
                if s.config_snapshot()? != *before
                    || s.state
                        .active()?
                        .iter()
                        .any(|r| operation["purge_data"] == true || r["harness"] != "human")
                {
                    return Err("STALE_GENERATION: Configuration or active bindings changed during disconnect".into());
                }
                Ok(())
            };
            let s = (*guard).borrow_mut();
            let admission = ready(s);
            let plugins: Vec<_> = s.plugins.agent_plugins().into_iter().cloned().collect();
            drop(guard);
            let removed = admission.and_then(|()| {
                plugins
                    .iter()
                    .try_for_each(|p| remove_plugin_setup(p).and_then(check_setup_actions))
            });
            guard = owner
                .lock()
                .map_err(|_| "Daemon lock poisoned during disconnect")?;
            let s = (*guard).borrow_mut();
            for p in plugins {
                s.invalidate_setup(&p.id);
            }
            match removed
                .and_then(|()| ready(s))
                .and_then(|()| s.begin_disconnect(operation, before))
            {
                Err(error) => result = Err(error),
                Ok(None) => {}
                Ok(Some(workers)) => {
                    // The reset owns admission until all retired workers have exited.
                    let mut activity = (*guard).borrow_mut().session_activity.take();
                    drop(guard);
                    for worker in workers {
                        let _ = worker.join();
                    }
                    let shutdown = activity.as_mut().map(|a| a.shutdown()).transpose();
                    guard = owner
                        .lock()
                        .map_err(|_| "Daemon lock poisoned during reset")?;
                    let s = (*guard).borrow_mut();
                    s.session_activity = Some(crate::core::session_activity::SessionActivity::new(
                        &s.config, &s.plugins,
                    ));
                    let purge = shutdown.and_then(|_| s.finish_purge());
                    s.purging = false;
                    if let Err(error) = purge {
                        result = Err(format!("RESET_INCOMPLETE: Configuration reset committed; local data purge failed: {error}"));
                    }
                }
            }
        }
        let s = (*guard).borrow_mut();
        let result = result
            .map(|value| match string(operation, "op") {
                "setup_disconnect_all" => committed_projection(value, s.reload_slots()),
                "setup_uninstall_all" => {
                    s.uninstall = Some(operation["purge_data"].as_bool().unwrap_or(false));
                    request_shutdown();
                    value
                }
                _ => value,
            })
            .map(|mut value| {
                if mutating_action(action) && value["ok"] != false {
                    value["ok"] = json!(true);
                    value["committed"] = json!(true);
                }
                if let Some(name) = resolved_hub {
                    value["redirect_port"] = s
                        .slots
                        .get(name)
                        .map(|slot| slot.config["redirectPort"].clone())
                        .unwrap_or(Value::Null);
                }
                value
            });
        if ingress_ui != s.ui_generation {
            return result.map(|v| {
                committed_projection(v, Err("STALE_GENERATION: UI changed during reset".into()))
            });
        }
        if action["op"] == "send_signal" {
            s.emit_composer_result(action, &result);
        }
        s.record_hub_result(operation, &result, resolved_hub);
        if action["op"] == "set_host_hub" || (operation["op"] == "setup_save_hub" && result.is_ok())
        {
            s.emit_settings_receipt(action, &result);
        }
        if action["op"] != "paste_hub_invite" && !refreshes_setup(action) {
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
                s.emit(json!({"op":"state","ok":true,"committed":true,"source_op":action["op"],"request_id":action["request_id"],"warning":warning}));
            }
        }
        if result
            .as_ref()
            .is_ok_and(|value| value["committed"] == true)
            && !refreshes_setup(action)
        {
            s.push_ui_result(Some(action));
        } else {
            s.push_ui();
        }
        result
    }

    fn emit_settings_receipt(&self, action: &Value, result: &Result<Value, String>) {
        let mut frame = json!({"op":"settings", "ok":result.is_ok(), "source_op":action["op"], "request_id":action["request_id"], "settings":self.settings()});
        match result {
            Ok(value) => {
                frame["committed"] = json!(true);
                frame["warning"] = value["warning"].clone();
            }
            Err(error) => frame["error"] = json!(error),
        }
        self.emit(frame);
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
            "hub_action" | "setup_save_hub" | "setup_toggle_hub"
        ) {
            let saving = action["op"] == "setup_save_hub";
            let mut hubs = Vec::new();
            if saving {
                if let Some(hub) = resolved_hub {
                    hubs.push(hub.to_owned());
                }
                hubs.extend(edited_hub(action));
                let requested = string(action, "name").trim();
                if requested.chars().any(|c| c.is_ascii_alphanumeric()) {
                    hubs.push(normalize_slug(requested));
                }
            } else {
                hubs.push(
                    action["hub"]
                        .as_str()
                        .or_else(|| action["name"].as_str())
                        .filter(|h| !h.is_empty())
                        .unwrap_or(&self.config.default_hub)
                        .to_owned(),
                );
            }
            hubs.sort_unstable();
            hubs.dedup();
            for hub in hubs {
                match result {
                    Ok(_) => {
                        self.last_hub_errors.remove(&hub);
                    }
                    Err(error) => {
                        let transient = [
                            "NATS_UNAVAILABLE:",
                            "UNAVAILABLE:",
                            "INTERNAL_ERROR:",
                            "AUTH_UNAVAILABLE:",
                            "UNAUTHORIZED:",
                        ]
                        .iter()
                        .any(|prefix| error.starts_with(prefix));
                        self.last_hub_errors.insert(hub, (error.clone(), transient));
                    }
                }
            }
        }
    }

    fn prepare_directory(&mut self, action: &Value, hub: &str) -> Result<ToolCall, String> {
        self.directory_open = true;
        self.directory_request = self.directory_request.wrapping_add(1);
        if action["op"] == "open_directory" {
            self.refresh_available()?;
        }
        self.activity_filter = action["filter"].as_str().unwrap_or("all").into();
        self.activity_query = if action["mode"] == "activity" {
            string(action, "query").into()
        } else {
            String::new()
        };
        let mut args = json!({"query":string(action,"query"),"limit":50});
        if let Some(hub) = action["hub"].as_str().filter(|h| !h.is_empty()) {
            args["hub"] = json!(hub);
        }
        validate_tool_input("cito_find", &args)?;
        self.slot(hub)?;
        let harness = action["harness"]
            .as_str()
            .filter(|id| !id.is_empty() && *id != "all")
            .map(str::to_owned);
        Ok(ToolCall::Find {
            args,
            harness,
            mine: matches!(action["scope"].as_str(), Some("mine" | "self")),
        })
    }
    fn directory_result(&self, action: &Value, hub: &str, result: &Value) -> Result<Value, String> {
        let (activity, total, unread) = self
            .state
            .query_activity(&self.activity_filter, &self.activity_query)?;
        let (all_total, all_unread) =
            if self.activity_filter == "all" && self.activity_query.trim().is_empty() {
                (total, unread)
            } else {
                self.state.activity_counts()?
            };
        let activity = self.activity_dto(activity, &self.address_book());
        let warnings: Vec<_> = result["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|w| format!("{}: {}", string(w, "hub"), string(w, "message")))
            .collect();
        let frame = json!({"op":"directory","hub":hub,"request_id":action["request_id"],"query":string(action,"query"),"scope":action["scope"].as_str().unwrap_or("all"),"harness":action["harness"].as_str().unwrap_or("all"),"matched_count":result["matched_count"],"online_count":result["online_count"],"peers":result["listeners"],"warnings":warnings,"activity":activity,"activity_total":total,"activity_has_any":all_total>0,"activity_unread_count":unread,"activity_filter":self.activity_filter,"activity_query":self.activity_query,"unread_count":all_unread});
        self.emit(frame.clone());
        Ok(frame)
    }

    fn stop_hub_recovery(&mut self, hub: &str) {
        if let Some(pending) = self
            .hub_controls
            .get_mut(hub)
            .and_then(|c| c.recovery.take())
        {
            pending.cancelled.store(true, Ordering::SeqCst);
            self.retired_auth.push(pending);
        }
        // A retired credential writer must exit before a cache is moved or deleted.
        for pending in self.retired_auth.drain(..) {
            let _ = pending.worker.join();
        }
    }

    fn cancel_hub_interaction(&mut self, hub: &str) {
        if self.active_settings_hub.as_deref() == Some(hub) {
            self.active_settings_hub = None;
        }
        if let Some(attempt) = self.hub_controls.get_mut(hub).and_then(|c| c.login.take()) {
            attempt.cancelled.store(true, Ordering::SeqCst);
            // The lock guards no data; wait for any in-flight publication to finish.
            drop(attempt.serial.lock().unwrap_or_else(|e| e.into_inner()));
        }
    }

    fn prepare_action(
        &mut self,
        action: &Value,
        ui_generation: Option<u64>,
        recover: bool,
    ) -> Result<ToolCall, String> {
        if !action.is_object() {
            return Err("INVALID_INPUT: Action must be an object".into());
        }
        let op = string(action, "op");
        match op {
            "hub_action"
                if !matches!(
                    action["action"].as_str(),
                    Some("login" | "switch_account" | "disconnect" | "reconnect")
                ) =>
            {
                return Err("INVALID_INPUT: Unknown hub action".into())
            }
            "cli_human_device" if !action["attach"].is_boolean() => {
                return Err("INVALID_INPUT: attach must be a boolean".into())
            }
            "set_launch_at_login" if !action["enabled"].is_boolean() => {
                return Err("INVALID_INPUT: enabled must be a boolean".into())
            }
            "quit" | "close_settings" | "close_directory" | "close_menu" => {
                match op {
                    "quit" => request_shutdown(),
                    "close_settings" => self.active_settings_hub = None,
                    "close_directory" => self.directory_open = false,
                    _ => self.menu_open = false,
                }
                return Ok(ToolCall::Ready(json!({"ok":true})));
            }
            _ => {}
        }
        if matches!(op, "hub_action" | "setup_toggle_hub") {
            let hub = action_hub(action)?;
            if op == "setup_toggle_hub" && !action["enabled"].is_boolean() {
                return Err("INVALID_INPUT: enabled must be a boolean".into());
            }
            if hub != "local" && self.raw_config()?["hubs"].get(hub).is_none() {
                return Err("NOT_FOUND: Unknown hub".into());
            }
        }
        if matches!(op, "unlisten_session" | "unlisten_all") {
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
        if op == "setup_remove_hub"
            || (op == "hub_action" && action["action"] == "disconnect")
            || (op == "setup_toggle_hub" && action["enabled"] == false)
        {
            let hub = action["hub"]
                .as_str()
                .unwrap_or(&self.config.default_hub)
                .to_owned();
            self.cancel_hub_interaction(&hub);
        }
        if !recover {
            self.reload_slots()?;
        }
        if (op == "cli_human_device" && action["attach"] == false)
            || matches!(
                op,
                "set_auto_listen"
                    | "unlisten_all"
                    | "set_listen_mode"
                    | "unlisten_session"
                    | "setup_remove_app"
                    | "setup_disconnect_all"
                    | "setup_uninstall_all"
            )
        {
            return Ok(ToolCall::RegistrationEdit {
                hubs: self.registration_targets(action)?,
                action: action.clone(),
                ui_generation,
            });
        }
        let hub = action["hub"]
            .as_str()
            .unwrap_or(&self.config.default_hub)
            .to_owned();
        match op {
            "get_snapshot" => {
                self.refresh_available()?;
                self.observations
                    .retain(|key, _| !key.starts_with("directory:"));
                wake_daemon();
                return self.snapshot().map(ToolCall::Ready);
            }
            "open_menu" => {
                self.menu_open = true;
                self.refresh_available()?;
            }
            "cli_human_device" => {
                let slot = self.slot(&hub)?;
                let caller = json!({"harness":"human","session_id":"default","cwd":"","title":"Human","principal_id":slot.identity["principal_id"]});
                return Ok(ToolCall::Listen {
                    hub,
                    args: json!({"about":"Human"}),
                    caller,
                });
            }
            "set_scope" | "human_inbox_update" => {
                self.slot(&hub)?;
                return Ok(ToolCall::HubControl {
                    hub,
                    action: action.clone(),
                });
            }
            "send_signal" => {
                return Ok(ToolCall::Send {
                    hub,
                    args: human_send_args(action)?,
                    caller: Some(
                        json!({"harness":"human","session_id":"default","cwd":"","title":"Human"}),
                    ),
                })
            }
            "cli_rename" => {
                let caller = self
                    .caller(&action["meta"], true)?
                    .ok_or("BINDING_REQUIRED: Caller required")?;
                return Ok(ToolCall::Rename {
                    caller,
                    title: string(action, "title").to_owned(),
                });
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
                return Ok(ToolCall::Rename {
                    caller: row,
                    title: string(action, "title").to_owned(),
                });
            }
            "set_user" => {
                let user = string(action, "user");
                if !crate::plugins::validate_user(user) {
                    return Err("INVALID_INPUT: Invalid user".into());
                }
                self.mutate_config(|v| {
                    v["user"] = json!(user);
                    validate_hub_endpoints(v, self.host_server.as_ref().map(|s| s.address), None)
                })?;
            }
            "setup_toggle_hub" | "hub_action" => {
                let enabled = if op == "setup_toggle_hub" {
                    action["enabled"] == true
                } else {
                    string(action, "action") != "disconnect"
                };
                self.mutate_config(|v| {
                    if v["hubs"].is_null() {
                        v["hubs"] = json!({});
                    }
                    if !v["hubs"].is_object() {
                        return Err("INVALID_INPUT: Hubs must be an object".into());
                    }
                    if v["hubs"][&hub].is_null() {
                        if hub != "local" {
                            return Err("NOT_FOUND: Unknown hub".into());
                        }
                        v["hubs"][&hub] = json!({"url":"local://"});
                    }
                    set_profile_enabled(
                        v,
                        &hub,
                        enabled,
                        self.host_server.as_ref().map(|server| server.address),
                    )
                })?;
                if op == "hub_action" && action["action"] == "reconnect" {
                    if let Some(old) = self.slots.remove(&hub) {
                        old.close_and_drain()?;
                    }
                }
            }
            "setup_remove_hub" => {
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
                return Ok(ToolCall::Ready(committed_projection(
                    json!({"ok":true}),
                    projection,
                )));
            }
            "paste_hub_invite" => {
                let mut invite = parse_hub_invite(string(action, "invite"))?;
                if let Some(port) = invite
                    .get("redirectPort")
                    .or_else(|| invite.get("oidcRedirectPort"))
                    .cloned()
                {
                    invite["redirect_port"] = port;
                }
                self.emit(json!({"op":"hub_invite","source_op":"paste_hub_invite","request_id":action["request_id"],"config":invite}));
                return Ok(ToolCall::Ready(json!({"ok":true})));
            }
            "ack_signals" => {
                let bulk = action["ack_all"] == true;
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
                return Ok(ToolCall::Ready(json!({"ok":true})));
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
                return Ok(ToolCall::Listen { hub, args, caller });
            }
            "open_directory" | "query_directory" => {
                return self.prepare_directory(action, &hub);
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
                    self.slot(from)?;
                    return Ok(ToolCall::Switch {
                        from: from.into(),
                        to: to.into(),
                        row,
                    });
                }
            }
            "set_host_hub" => {
                let mut server = self.host_server.take();
                let previous = server
                    .as_ref()
                    .map(|server| (server.address.ip(), server.port(), server.auth.clone()));
                let mut applied = false;
                let result = self.mutate_config(|v| {
                    let (_, _, rest) =
                        crate::plugins::try_normalize_existing_config(Some(&v.to_string()))?;
                    v["host_hub"] =
                        apply_host_hub_patch(rest.get("host_hub").unwrap_or(&Value::Null), action)?;
                    let settings = host_hub_server_config(&v["host_hub"])?;
                    let ephemeral = settings.as_ref().is_some_and(|(_, port, _)| *port == 0);
                    if !ephemeral {
                        validate_hub_endpoints(
                            v,
                            settings
                                .as_ref()
                                .map(|(ip, port, _)| std::net::SocketAddr::new(*ip, *port)),
                            None,
                        )?;
                    }
                    if !self.closing {
                        replace_host_hub_server(&self.local, &mut server, settings)?;
                        applied = true;
                    }
                    if let Some(m) = v.as_object_mut() {
                        m.remove("hostHub");
                    }
                    if ephemeral {
                        validate_hub_endpoints(v, server.as_ref().map(|s| s.address), None)?;
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
            }
            "set_launch_at_login" => {
                set_launch_at_login(
                    action["enabled"] == true,
                    &self.config.state_dir,
                    &self.config.home_dir,
                    &self.plugins,
                )?;
            }
            "refresh_available" => {
                self.refresh_available()?;
                self.poll()?;
                // A CLI setup refresh has no requesting UI surface. Let finish_action
                // publish the normal open/closed projection after poll reloaded the slots.
                if ui_generation.is_none() && action["request_id"].is_null() {
                    return Ok(ToolCall::Ready(json!({"ok":true})));
                }
            }
            _ => return Err(format!("INVALID_INPUT: Unknown UI action: {op}")),
        }
        if matches!(
            op,
            "set_user" | "setup_toggle_hub" | "hub_action" | "set_host_hub"
        ) {
            let reload = self.reload_slots();
            return Ok(ToolCall::Ready(committed_projection(
                json!({"ok":true}),
                reload,
            )));
        }
        if !mutating_action(action) {
            self.push_ui_result(Some(action));
        }
        Ok(ToolCall::Ready(json!({"ok":true})))
    }

    pub(super) fn registration_targets(&self, action: &Value) -> Result<Vec<String>, String> {
        if action["op"] == "cli_human_device" {
            let hub = action["hub"].as_str().unwrap_or(&self.config.default_hub);
            self.session_node_id(hub, "human", "default")?;
            return Ok(vec![hub.into()]);
        }
        if let Some(hub) = action["hub"]
            .as_str()
            .filter(|_| !string(action, "op").starts_with("setup_"))
        {
            // Reject unknown input before allocating a persistent lock registry entry.
            if matches!(string(action, "op"), "set_auto_listen" | "set_listen_mode") {
                self.slot(hub)?;
            } else {
                self.validate_configured_hub(hub)?;
            }
            return Ok(vec![hub.into()]);
        }
        if matches!(string(action, "op"), "set_auto_listen" | "set_listen_mode") {
            return Ok(vec![self.config.default_hub.clone()]);
        }
        let mut hubs: std::collections::HashSet<_> = self.hub_controls.keys().cloned().collect();
        hubs.extend(self.slots.keys().cloned());
        hubs.extend(
            self.state
                .db
                .query("SELECT DISTINCT hub FROM local_listeners", &[])?
                .iter()
                .map(|r| string(r, "hub").to_owned()),
        );
        let mut hubs: Vec<_> = hubs.into_iter().collect();
        hubs.sort();
        Ok(hubs)
    }

    pub(super) fn apply_registration_edit(
        &mut self,
        action: &Value,
        hub: String,
    ) -> Result<ToolCall, String> {
        let op = string(action, "op");
        let mut detaches = Detaches::default();
        match op {
            "cli_human_device" => {
                let id = self.session_node_id(&hub, "human", "default")?;
                let removed = detaches.retire(self, &hub, &id, "user")?;
                Ok(detaches.settle(json!({"ok":true,"removed":u8::from(removed)}), true))
            }
            "unlisten_session" => {
                let mut detach_errors = Vec::new();
                if action["harness"] != "human" {
                    let targets: Vec<String> = self
                        .session_hubs(string(action, "harness"), string(action, "session_id"))?
                        .into_iter()
                        .filter(|name| action["hub"].is_null() || *name == hub)
                        .collect();
                    for name in targets {
                        self.ensure_agent_stop_row_locked(
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
                            if let Err(error) =
                                detaches.retire(self, &name, string(&row, "listener_id"), "user")
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
                        if let Err(error) = detaches.retire(self, &name, &id, "user") {
                            detach_errors.push(format!("{name}: {error}"));
                        }
                    }
                }
                if !detach_errors.is_empty() {
                    return Err(format!("DETACH_PENDING: {}", detach_errors.join("; ")));
                }
                Ok(detaches.settle(json!({"ok":true}), true))
            }
            "setup_remove_app" => {
                let id = action["harness"]
                    .as_str()
                    .ok_or("INVALID_INPUT: harness required")?;
                let p = self
                    .plugins
                    .find_by_id(id)
                    .ok_or("UNSUPPORTED_HARNESS: Unknown harness")?;
                let notice = p
                    .post_disconnect_hint
                    .clone()
                    .unwrap_or("Disconnected · Active listeners stopped".into());
                let mut detach_errors = Vec::new();
                for row in self
                    .state
                    .active()?
                    .into_iter()
                    .filter(|r| r["harness"] == id)
                {
                    if let Err(error) = detaches.retire(
                        self,
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
                if let Some(activity) = self.session_activity.as_mut() {
                    activity.remove_candidates(id);
                }
                let mut result = json!({"ok":true,"notice":notice,"detach_status":if detach_errors.is_empty(){"complete"}else{"pending"},"warnings":detach_errors});
                if !detach_errors.is_empty() {
                    result["warning"] = json!(detach_errors.join("; "));
                }
                Ok(detaches.settle(result, false))
            }
            "unlisten_all" => {
                // Bulk stop owns Hub policy even when no listener is active.
                let selected_hub = action.get("hub").filter(|hub| !hub.is_null());
                let (targets, damaged) = match configured_hubs(&self.config_snapshot()?) {
                    Ok(configured) => (
                        configured
                            .into_iter()
                            .map(|(name, _)| name)
                            .filter(|name| selected_hub.is_none_or(|hub| hub == name.as_str()))
                            .collect::<Vec<_>>(),
                        false,
                    ),
                    Err(_) if selected_hub.is_none() => (self.registration_targets(action)?, true),
                    Err(error) => return Err(error),
                };
                if targets.is_empty() && action["hub"].is_string() {
                    return Err("INVALID_INPUT: Unknown hub".into());
                }
                let mut removed = 0;
                let mut detach_errors = Vec::new();
                for row in self.state.active()?.into_iter().filter(|row| {
                    row["harness"] != "human" && selected_hub.is_none_or(|hub| row["hub"] == *hub)
                }) {
                    match detaches.retire(
                        self,
                        string(&row, "hub"),
                        string(&row, "listener_id"),
                        "user",
                    ) {
                        Ok(true) => removed += 1,
                        Ok(false) => {}
                        Err(error) => {
                            detach_errors.push(format!("{}: {error}", string(&row, "hub")))
                        }
                    }
                }
                for name in targets {
                    self.state.db.execute("UPDATE local_listeners SET user_stopped=1,retirement_reason=NULL WHERE hub=? AND harness!='human' AND status!='FENCED_INACTIVE'", &[json!(name)])?;
                    // A damaged profile cannot safely receive a policy patch. Keep
                    // the file intact and stop enrollment in the running authority.
                    if damaged {
                        if let Some(slot) = self.slots.get_mut(&name) {
                            slot.config["autoListen"] = json!(false);
                        }
                        continue;
                    }
                    // Bulk stop reports incomplete detaches across all targets.
                    if let Err(error) = self
                        .set_auto_listen_locked(&name, false, &mut detaches)
                        .and_then(|projection| projection)
                    {
                        detach_errors.push(format!("{name}: {error}"));
                    }
                }
                if !detach_errors.is_empty() {
                    return Err(format!("DETACH_PENDING: {}", detach_errors.join("; ")));
                }
                Ok(detaches.settle(json!({"ok":true,"removed":removed}), true))
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
                Ok(ToolCall::Ready(json!({"ok":true,"committed":true})))
            }
            "setup_disconnect_all" | "setup_uninstall_all" => {
                let uninstall = op == "setup_uninstall_all";
                for row in self.state.active()?.into_iter().filter(|row| {
                    uninstall || action["purge_data"] == true || row["harness"] != "human"
                }) {
                    let result = detaches.retire(
                        self,
                        string(&row, "hub"),
                        string(&row, "listener_id"),
                        "disconnected",
                    );
                    if !uninstall {
                        result?;
                    }
                }
                Ok(detaches.settle(json!({"ok":true}), !uninstall))
            }
            "set_auto_listen" => {
                if !self.slots.contains_key(&hub) {
                    return Err("INVALID_INPUT: Enabled hub required".into());
                }
                let enabled = action["enabled"]
                    .as_bool()
                    .ok_or("INVALID_INPUT: enabled must be a boolean")?;
                let projection = self.set_auto_listen_locked(&hub, enabled, &mut detaches)?;
                Ok(detaches.settle(committed_projection(json!({"ok":true}), projection), false))
            }
            _ => Err(format!("INVALID_INPUT: Unknown registration edit: {op}")),
        }
    }

    fn begin_disconnect(
        &mut self,
        action: &Value,
        before: &ConfigSnapshot,
    ) -> Result<Option<Vec<std::thread::JoinHandle<()>>>, String> {
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
            // The explicit purge replaces Hub settings without needing to parse them.
            // Retain unrelated settings whenever the original object can be decoded.
            // finish_purge resets all channel processes and their persistent state.
            let target = crate::support::publication_target(&self.config.config_path)?;
            crate::plugins::with_config_lock(&target, || {
                before.reset_hubs_locked(&self.config.config_path, &target)
            })?;
            let logins = self
                .hub_controls
                .values_mut()
                .filter_map(|control| control.login.take())
                .collect::<Vec<_>>();
            for attempt in &logins {
                attempt.cancelled.store(true, Ordering::SeqCst);
            }
            for attempt in &logins {
                drop(attempt.serial.lock().map_err(|_| "Login lock poisoned")?);
            }
            for slot in self.slots.values() {
                slot.close_and_drain()?;
            }
            self.events
                .extend(self.receivers.drain().map(|(_, worker)| worker));
            self.purging = true;
            return Ok(Some(std::mem::take(&mut self.events)));
        }
        self.clear_disconnected_candidates();
        Ok(None)
    }

    fn clear_disconnected_candidates(&mut self) {
        if let Some(activity) = self.session_activity.as_mut() {
            for plugin in self.access.plugins.agent_plugins() {
                activity.remove_candidates(&plugin.id);
            }
        }
    }

    fn finish_purge(&mut self) -> Result<(), String> {
        self.slots.clear();
        self.channel_inventories.clear();
        self.active_settings_hub = None;
        if let Some(server) = self.host_server.take() {
            server.shutdown();
        }
        self.cancel_logins();
        self.join_recoveries();
        self.local.reset_data()?;
        crate::setup::purge_disconnected_data(&self.config.state_dir, Some(&self.state))?;
        self.clear_disconnected_candidates();
        Ok(())
    }
}
