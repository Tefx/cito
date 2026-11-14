use super::{
    projection::{directory_identity, wire_status},
    shutdown_requested, DaemonService,
};
use crate::core::{
    identity::{hash, iso, listener_id, local_host_name, normalize_slug, sanitize_message, string},
    receiver::plugin_request,
    registration::{distinct_label, is_generic_session_label},
};
use crate::plugins::{now_ms, random_uuid_v4};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub(super) fn default_session_title(
    caller: &Value,
    home: &std::path::Path,
    harness_name: &str,
    user: &str,
    about: &str,
) -> String {
    let raw = string(caller, "title");
    let title = raw.trim();
    let home_basename = home
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    // Home-directory sessions may already carry a promoted meaningful title.
    if caller["harness"] == "human" || !is_generic_session_label(title, user, home_basename) {
        return raw.to_owned();
    }
    let about = about.trim();
    distinct_label(about, title, user, home_basename)
        .unwrap_or(harness_name)
        .to_owned()
}

fn caller_pid_hint(meta: &Value) -> Result<Option<u64>, String> {
    let mut pending = vec![(meta, 0usize)];
    let mut pid = None;
    let mut visited = 0;
    while let Some((value, depth)) = pending.pop() {
        visited += 1;
        if visited > 512 {
            return Err("INVALID_INPUT: Caller metadata is too complex".into());
        }
        if depth > 8 {
            continue;
        }
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    if key != "http_client_pid"
                        && (key == "pid"
                            || key.ends_with("_pid")
                            || key.ends_with("Pid")
                            || key.ends_with("PID"))
                        && !value.is_null()
                    {
                        let next = value
                            .as_u64()
                            .filter(|value| *value > 0 && *value <= i32::MAX as u64)
                            .ok_or("INVALID_INPUT: Caller PID must be a positive process ID")?;
                        if pid.is_some_and(|previous| previous != next) {
                            return Err("INVALID_INPUT: Conflicting caller PID claims".into());
                        }
                        pid = Some(next);
                    }
                    if value.is_object() || value.is_array() {
                        pending.push((value, depth + 1));
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    pending.push((value, depth + 1));
                }
            }
            _ => {}
        }
    }
    Ok(pid)
}

pub(super) fn validate_tool_input(name: &str, args: &Value) -> Result<(), String> {
    let map = args
        .as_object()
        .ok_or("INVALID_INPUT: Tool arguments must be an object")?;
    let allowed: &[&str] = match name {
        "cito_listen" => &["about", "title", "tag", "hub", "scope"],
        "cito_status" => &["hub"],
        "cito_unlisten" => &["hub"],
        "cito_find" => &["query", "hub", "user", "host", "harness", "repo", "limit"],
        "cito_send" => &["to", "hub", "message", "in_reply_to", "idempotency_key"],
        "cito_inbox" => &["hub", "ack_ids", "ack_all", "limit"],
        _ => return Err("INVALID_INPUT: Unknown tool".into()),
    };
    for (key, value) in map {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("INVALID_INPUT: Unknown argument: {key}"));
        }
        let valid = match key.as_str() {
            "limit" => value
                .as_u64()
                .is_some_and(|n| n > 0 && n <= if name == "cito_find" { 50 } else { 1000 }),
            "ack_all" | "wake" => value.is_boolean(),
            "ack_ids" => value.as_array().is_some_and(|a| {
                a.len() <= 1000
                    && a.iter()
                        .all(|v| v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 161))
            }),
            _ => value.is_string(),
        };
        if !valid {
            return Err(format!("INVALID_INPUT: Invalid argument: {key}"));
        }
    }
    if name == "cito_listen"
        && !args["about"]
            .as_str()
            .is_some_and(|about| !about.trim().is_empty() && about.chars().count() <= 500)
    {
        return Err("INVALID_INPUT: about must contain 1..500 characters".into());
    }
    if let Some(title) = args["title"].as_str() {
        crate::core::registration::validate_title(title)?;
    }
    if let Some(scope) = args["scope"].as_str() {
        if !matches!(scope, "self" | "workspace") {
            return Err("INVALID_INPUT: Invalid scope".into());
        }
    }
    if name == "cito_send" && (!args["to"].is_string() || !args["message"].is_string()) {
        return Err("INVALID_INPUT: to and message are required".into());
    }
    if name == "cito_send" {
        let raw = string(args, "to");
        let (prefix, address) = raw
            .split_once(':')
            .map_or((None, raw), |(hub, address)| (Some(hub), address));
        if prefix.is_some_and(|hub| !crate::plugins::validate_hub_name(hub)) {
            return Err("INVALID_INPUT: Invalid scoped Hub".into());
        }
        if let Some(handle) = address.strip_prefix('@') {
            if handle.is_empty()
                || handle.chars().count() > 64
                || handle.chars().any(|ch| {
                    ch.is_whitespace() || ch.is_control() || matches!(ch, ':' | '@' | '/')
                })
            {
                return Err("INVALID_INPUT: Invalid Human address".into());
            }
        } else if crate::core::identity::readable_alias(address).is_none() {
            crate::core::identity::canonical_node_id(address)?;
        }
    }
    if let Some(hub) = args["hub"].as_str() {
        if !crate::plugins::validate_hub_name(hub) {
            return Err("INVALID_INPUT: Invalid hub".into());
        }
    }
    if let Some(harness) = args["harness"].as_str() {
        if !crate::plugins::validate_harness_id(harness) {
            return Err("UNSUPPORTED_HARNESS: Invalid harness".into());
        }
    }
    for (key, cap) in [
        ("tag", 64),
        ("query", 200),
        ("user", 64),
        ("host", 128),
        ("repo", 200),
        ("in_reply_to", 161),
        ("idempotency_key", 128),
    ] {
        if let Some(value) = args[key].as_str() {
            if value.chars().count() > cap
                || matches!(key, "tag" | "in_reply_to" | "idempotency_key")
                    && value.trim().is_empty()
            {
                return Err(format!("INVALID_INPUT: Invalid {key} length"));
            }
        }
    }
    Ok(())
}

pub(super) fn tools_list() -> Value {
    let mut tools = json!({"tools":[
     {"name":"cito_status","description":"Inspect this session's current Cito registration, address, connected hubs, and unread inbox count.","inputSchema":{"type":"object","properties":{"hub":{"type":"string"}}}},
     {"name":"cito_listen","description":"Enroll this session in the lightweight control plane for signaling and wake notifications.","inputSchema":{"type":"object","properties":{"about":{"type":"string","minLength":1,"maxLength":500},"title":{"type":"string","minLength":1,"maxLength":128},"tag":{"type":"string","maxLength":64},"hub":{"type":"string"},"scope":{"type":"string","enum":["self","workspace"]}},"required":["about"]}},
     {"name":"cito_unlisten","description":"Revoke this session on one or all hubs.","inputSchema":{"type":"object","properties":{"hub":{"type":"string"}}}},
     {"name":"cito_find","description":"Find opted-in active nodes.","inputSchema":{"type":"object","properties":{"hub":{"type":"string"},"query":{"type":"string","maxLength":200},"user":{"type":"string","maxLength":64},"host":{"type":"string","maxLength":128},"harness":{"type":"string","pattern":"^[a-z0-9][a-z0-9_-]{0,31}$"},"repo":{"type":"string","maxLength":200},"limit":{"type":"integer","minimum":1,"maximum":50}}}},
     {"name":"cito_send","description":"Send a lightweight control plane signal of 1..65,536 Unicode code points. Prefer external channels for long debates or large artifacts.","inputSchema":{"type":"object","properties":{"to":{"type":"string","pattern":"^(?:[a-z0-9][a-z0-9_-]{0,31}:)?(?:c_(?:[a-f0-9]{16}|[a-z2-7]{12})|[a-z0-9]+(?:-[a-z0-9]+)*-(?:[a-f0-9]{16}|[a-z2-7]{12})|@[a-zA-Z0-9._-]{1,64}|[a-zA-Z0-9._-]{1,64}(?:/[a-z0-9][a-z0-9_-]{0,31})?#[a-z0-9]{1,16})$"},"message":{"type":"string","minLength":1,"maxLength":65536},"hub":{"type":"string"},"in_reply_to":{"type":"string","minLength":1,"maxLength":161},"idempotency_key":{"type":"string","minLength":1,"maxLength":128}},"required":["to","message"]}},
     {"name":"cito_inbox","description":"Read and acknowledge this node's inbox.","inputSchema":{"type":"object","properties":{"hub":{"type":"string"},"ack_all":{"type":"boolean"},"ack_ids":{"type":"array","items":{"type":"string","minLength":1,"maxLength":161}},"limit":{"type":"integer","minimum":1,"maximum":1000}}}}
    ]});
    if let Some(list) = tools["tools"].as_array_mut() {
        for tool in list {
            tool["inputSchema"]["additionalProperties"] = json!(false);
        }
    }
    tools
}

pub(super) fn wait_send_receipt(
    service: &Arc<Mutex<DaemonService>>,
    payload: &mut Value,
) -> Result<(), String> {
    if payload["status"] == "PENDING_PUBLISH" {
        return Ok(());
    }
    let (state, slot, signal) = {
        let s = service.lock().map_err(|_| "Service lock poisoned")?;
        let raw = string(payload, "signal_id");
        let (hub, signal) = raw.split_once(':').unwrap_or((&s.config.default_hub, raw));
        (s.state.clone(), s.slot(hub)?.clone(), signal.to_string())
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if shutdown_requested() || slot.closed.load(Ordering::SeqCst) {
            return Err("Send interrupted".into());
        }
        let remote = slot.rpc(
            "stream_get_message",
            &json!({"stream":"CITO_SIGNALS","seq":payload["seq"]}),
        )?;
        if remote["received"] == true {
            break;
        }
        if Instant::now() >= deadline {
            payload["status"] = json!("stored_passive");
            payload["held_reason"] = json!("sender_ack_timeout");
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    state.db.execute(
        "UPDATE local_sent_idempotency SET status=?,held_reason=? WHERE hub=? AND signal_id=?",
        &[
            payload["status"].clone(),
            payload["held_reason"].clone(),
            json!(slot.name),
            json!(signal),
        ],
    )?;
    Ok(())
}

impl DaemonService {
    pub(super) fn caller(&self, meta: &Value, required: bool) -> Result<Option<Value>, String> {
        let requested_pid = caller_pid_hint(meta)?.or_else(|| meta["_http_client"].as_u64());
        let explicit_pid = meta
            .as_object()
            .is_some_and(|m| m.keys().any(|k| k.ends_with("_pid") || k.ends_with("Pid")));
        let expected = string(meta, "expected_harness");
        if expected == "human" {
            if !matches!(string(meta, "expected_session_id"), "" | "default") {
                return Err("INVALID_INPUT: Human node session_id must be 'default'".into());
            }
            return Ok(Some(
                json!({"harness":"human","session_id":"default","cwd":"","title":"Human"}),
            ));
        }
        let mut failures = Vec::new();
        for plugin in self.plugins.agent_plugins() {
            if !expected.is_empty() && plugin.id != expected {
                continue;
            }
            let mut request = json!({"op":"resolve_caller","meta":meta,"headers":meta["headers"],"cwd":meta["cwd"],"http_transport":true,"http_client_pid":requested_pid,"require_open":required,"explicit_session_id":meta["expected_session_id"],"cli_cmd":meta["cli_cmd"],"state_dir":self.config.state_dir,"env":{}});
            if !required {
                if let Some(cached) = requested_pid.and_then(|pid| {
                    self.caller_cache.lock().ok().and_then(|cache| {
                        cache
                            .get(&pid)
                            .filter(|caller| caller["harness"] == plugin.id)
                            .cloned()
                    })
                }) {
                    request["cached_caller"] = cached;
                }
            }
            match plugin_request(plugin, &request, None, None) {
                Ok(reply) if reply["ok"] == true => {
                    let caller = reply["caller"].clone();
                    if string(&caller, "harness") != plugin.id
                        || string(&caller, "session_id").is_empty()
                    {
                        return Err(
                            "BINDING_REQUIRED: Provider returned an invalid caller identity".into(),
                        );
                    }
                    if !string(meta, "expected_session_id").is_empty()
                        && caller["session_id"] != meta["expected_session_id"]
                    {
                        return Err("BINDING_REQUIRED: Caller session mismatch".into());
                    }
                    if let Some(pid) = caller["endpoint_pid"]
                        .as_u64()
                        .filter(|pid| *pid > 0 && *pid <= i32::MAX as u64)
                    {
                        if let Ok(mut cache) = self.caller_cache.lock() {
                            cache.retain(|pid, _| crate::plugins::pid_alive(*pid as i32));
                            cache.insert(pid, caller.clone());
                            if let Some(requested_pid) = requested_pid {
                                cache.insert(requested_pid, caller.clone());
                            }
                        }
                    }
                    return Ok(Some(caller));
                }
                Ok(reply) if reply["error_code"] == "INVALID_INPUT" => {
                    return Err(format!("INVALID_INPUT: {}", string(&reply, "message")));
                }
                Ok(reply) => {
                    if explicit_pid && reply["error_code"] != "BINDING_REQUIRED" {
                        return Err(format!(
                            "{}: {}",
                            string(&reply, "error_code"),
                            string(&reply, "message")
                        ));
                    }
                    failures.push(reply)
                }
                Err(error) => {
                    failures.push(json!({"error_code":"BINDING_REQUIRED","message":error}))
                }
            }
        }
        if !expected.is_empty() || explicit_pid || !string(meta, "expected_session_id").is_empty() {
            return Err(failures
                .first()
                .map(|reply| {
                    format!(
                        "{}: {}",
                        string(reply, "error_code"),
                        string(reply, "message")
                    )
                })
                .unwrap_or_else(|| "UNSUPPORTED_HARNESS: Unknown harness".into()));
        }
        if required {
            return Err("BINDING_REQUIRED: No caller socket bridge found".into());
        }
        Ok(None)
    }

    pub(super) fn rename_session(
        &self,
        harness: &str,
        session: &str,
        title: &str,
    ) -> Result<Value, String> {
        if harness.is_empty() || harness == "human" || session.is_empty() {
            return Err("INVALID_INPUT: An agent session is required".into());
        }
        self.state.set_custom_title(harness, session, title)?;
        let mut warnings = Vec::new();
        for row in self
            .state
            .active()?
            .into_iter()
            .filter(|r| r["harness"] == harness && r["session_id"] == session)
        {
            if let Some(slot) = self.slots.get(string(&row, "hub")) {
                if let Err(error) = crate::core::registration::sync_custom_title(
                    &self.state,
                    slot,
                    &self.host_id,
                    &row,
                ) {
                    warnings.push(json!({"hub":row["hub"],"message":error}));
                }
            }
        }
        Ok(json!({"ok":true,"title":title,"warnings":warnings}))
    }

    pub(super) fn listen(&self, hub: &str, args: &Value, caller: &Value) -> Result<Value, String> {
        let mut caller = caller.clone();
        if caller["harness"] != "human"
            && self
                .state
                .custom_title(string(&caller, "harness"), string(&caller, "session_id"))?
                .is_none()
            && !args["title"].is_string()
        {
            let name = self
                .plugins
                .find_by_id(string(&caller, "harness"))
                .map(|p| p.display_name.as_str())
                .unwrap_or(string(&caller, "harness"));
            caller["title"] = json!(default_session_title(
                &caller,
                &self.config.home_dir,
                name,
                string(&self.slot(hub)?.identity, "owner_user"),
                string(args, "about")
            ));
        }
        let caller = &caller;
        let previous: Vec<Value> = self
            .state
            .active()?
            .into_iter()
            .filter(|row| {
                row["hub"] == hub
                    && row["harness"] == caller["harness"]
                    && row["session_id"] != caller["session_id"]
                    && caller["endpoint_pid"].is_number()
                    && row["endpoint_pid"] == caller["endpoint_pid"]
            })
            .collect();
        let mut result = crate::core::registration::register_listener(
            &self.state,
            self.slot(hub)?,
            &self.host_id,
            args,
            caller,
        )?;
        // The new remote/local binding commits before the previous endpoint is revoked.
        for row in previous {
            self.retire_id(hub, string(&row, "listener_id"), "hard_exit")?;
        }
        // Registration commits the Hub-scoped PID and transition timestamp together.
        // Do not rewrite binding metadata after releasing registration authority.
        let _ = self.slot(hub)?.directory();
        result["listener_id"] = json!(self.scoped(hub, string(&result, "listener_id")));
        Ok(result)
    }

    /// Stop intent exists even if this session has never been enrolled.
    pub(super) fn ensure_agent_stop_row(
        &self,
        hub: &str,
        harness: &str,
        session: &str,
    ) -> Result<String, String> {
        if harness.is_empty() || harness == "human" || session.is_empty() {
            return Err("INVALID_INPUT: An agent session is required".into());
        }
        let slot = self.slot(hub)?;
        let lock = self.state.registration_lock(hub)?;
        let _guard = lock
            .lock()
            .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
        let id = super::super::registration::local_node_id(&self.state, slot, harness, session)?;
        self.state.db.execute("INSERT OR IGNORE INTO local_listeners(hub,listener_id,public_id,principal_id,owner_user,host_id,host_name,harness,session_id,cwd,title,about,scope,status,user_stopped,retirement_reason,hub_url) VALUES(?,?,?,?,?,?,?,?,?,'','','','self','UNLISTENED',1,'user',?)", &[json!(hub),json!(id),json!(crate::core::identity::public_listener_id(string(&slot.identity,"owner_user"),&id)),slot.identity["principal_id"].clone(),slot.identity["owner_user"].clone(),json!(self.host_id),json!(local_host_name()),json!(harness),json!(session),slot.config["url"].clone()])?;
        Ok(id)
    }

    pub(super) fn pause_listener(&mut self, hub: &str, id: &str) -> Result<bool, String> {
        self.unlisten_id(hub, id)
    }

    pub(super) fn unlisten_id(&self, hub: &str, id: &str) -> Result<bool, String> {
        self.retire_id(hub, id, "user")
    }

    pub(super) fn retire_id(&self, hub: &str, id: &str, reason: &str) -> Result<bool, String> {
        let client = self.slots.get(hub);
        let hub_url = if let Some(client) = client {
            Some(string(&client.config, "url").to_owned())
        } else {
            // Disabled Hubs have no live slot; their normalized configuration
            // still identifies the only target a deferred detach may affect.
            let raw = self.raw_config()?;
            let (_, hubs, _) =
                crate::plugins::try_normalize_existing_config(Some(&raw.to_string()))?;
            hubs.get(hub).map(|config| string(config, "url").to_owned())
        };
        let registration_lock = self.state.registration_lock(hub)?;
        let registration = registration_lock
            .lock()
            .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
        let row = self.state.listener(hub, id)?;
        let active = row.as_ref().is_some_and(|row| row["status"] == "ACTIVE");
        self.state.db.transaction(|c| {
            if row.is_some() {
                c.execute("UPDATE local_signals SET state='STORED_PASSIVE',held_reason='unlistened',dispatcher_token=NULL WHERE hub=? AND listener_id=? AND acknowledged_at_ms IS NULL AND state IN ('RECEIVED','DISPATCHING')", &[json!(hub),json!(id)])?;
                c.execute("UPDATE local_listeners SET status='UNLISTENED',updated_at=?,user_stopped=CASE WHEN harness!='human' AND ?='user' THEN 1 ELSE user_stopped END,retirement_reason=CASE WHEN harness!='human' THEN ? ELSE NULL END,retired_activity_revision=CASE WHEN ?='idle' THEN last_activity_revision ELSE retired_activity_revision END,dispatcher_token=NULL,lease_until_ms=0 WHERE hub=? AND listener_id=?", &[json!(now_ms()),json!(reason),json!(reason),json!(reason),json!(hub),json!(id)])?;
            } else if let Some(client) = client.filter(|client|id==listener_id(string(&client.identity,"principal_id"),"human","default")) {
                // Persist intent before queued automatic enrollment can obtain the lock.
                c.execute("INSERT OR IGNORE INTO local_listeners(hub,listener_id,public_id,principal_id,owner_user,host_id,host_name,harness,session_id,cwd,title,about,scope,status) VALUES(?,?,?,?,?,?,?,'human','default','','','Human','self','UNLISTENED')",&[
                    json!(hub),json!(id),json!(crate::core::identity::public_listener_id(string(&client.identity,"owner_user"),id)),
                    client.identity["principal_id"].clone(),client.identity["owner_user"].clone(),json!(self.host_id),json!(local_host_name())])?;
            } else { return Ok(()); }
            if let Some(url) = &hub_url {
                c.execute("UPDATE local_listeners SET hub_url=COALESCE(hub_url,?) WHERE hub=? AND listener_id=?", &[json!(url),json!(hub),json!(id)])?;
                c.execute("INSERT INTO pending_hub_detaches(hub,listener_id,hub_url) VALUES(?,?,?) ON CONFLICT(hub,listener_id) DO UPDATE SET hub_url=excluded.hub_url",&[json!(hub),json!(id),json!(url)])?;
            }
            Ok(())
        })?;
        if let Some(row) = &row {
            self.state.invalidate_discovery(string(row, "harness"))?;
        }
        drop(registration);
        // A failed Hub write leaves durable pending work; poll/reconnect retries it.
        if let Some(client) = client {
            Self::reconcile_unlistened(&self.state, client, &self.host_id)?;
        } else if !self
            .state
            .db
            .query(
                "SELECT 1 FROM pending_hub_detaches WHERE hub=? AND listener_id=?",
                &[json!(hub), json!(id)],
            )?
            .is_empty()
        {
            return Err("DETACH_PENDING: Hub is disabled; remote detach remains pending".into());
        }
        Ok(active)
    }

    pub(super) fn find_listeners(
        &mut self,
        args: &Value,
        include: impl Fn(&Value) -> bool,
    ) -> Result<Value, String> {
        let hub = args["hub"]
            .as_str()
            .unwrap_or(&self.config.default_hub)
            .to_string();
        let limit = args["limit"].as_u64().unwrap_or(20);
        if !(1..=50).contains(&limit) {
            return Err("INVALID_INPUT: limit must be 1..50".into());
        }
        let query = string(args, "query").to_lowercase();
        let mut rows = Vec::new();
        let mut warnings = Vec::new();
        let mut retried = false;
        let snapshots = loop {
            let snapshots = thread::scope(|scope| {
                let workers: Vec<_> = self
                    .slots
                    .iter()
                    .filter(|(name, _)| !args["hub"].is_string() || *name == &hub)
                    .map(|(name, slot)| {
                        let worker = scope.spawn(move || slot.directory());
                        (name.clone(), slot.clone(), worker)
                    })
                    .collect();
                workers
                    .into_iter()
                    .map(|(name, slot, worker)| {
                        (
                            name,
                            slot,
                            worker.join().unwrap_or_else(|_| {
                                Err("NATS_UNAVAILABLE: Directory worker failed".into())
                            }),
                        )
                    })
                    .collect::<Vec<_>>()
            });
            if !retried
                && snapshots.iter().any(|(_, slot, result)| {
                    slot.config["authMode"] == "oidc"
                        && result.as_ref().is_err_and(|e| {
                            e.starts_with("UNAUTHORIZED:")
                                || e.starts_with("AUTHORIZATION_VIOLATION:")
                        })
                })
            {
                retried = true;
                self.reload_slots()?;
                continue;
            }
            break snapshots;
        };
        for (name, slot, snapshot) in snapshots {
            match snapshot {
                Ok(directory) => {
                    for row in directory {
                        if !matches!(
                            crate::core::lease::evaluate_node_lease(&row, now_ms()),
                            "ACTIVE" | "DISCONNECTED_RETAINED"
                        ) || (row["scope"] == "self"
                            && !crate::core::auth::principal_ids_equal(
                                string(&row, "principal_id"),
                                string(&slot.identity, "principal_id"),
                            ))
                        {
                            continue;
                        }
                        let host_matches = |term: &str| {
                            let term = term.to_lowercase();
                            (row["channels"].as_object().is_none_or(|c| c.is_empty())
                                && (string(&row, "host_name").to_lowercase().contains(&term)
                                    || string(&row, "host_id").to_lowercase().contains(&term)))
                                || crate::core::lease::attachments(&row, &self.host_id, now_ms())
                                    .iter()
                                    .any(|attachment| {
                                        string(attachment, "display_name")
                                            .to_lowercase()
                                            .contains(&term)
                                            || string(attachment, "host_id")
                                                .to_lowercase()
                                                .contains(&term)
                                    })
                        };
                        if ["user", "harness", "repo", "host"].iter().any(|key| {
                            let filter = string(args, key);
                            if *key == "host" {
                                return !filter.is_empty() && !host_matches(filter);
                            }
                            let field = match *key {
                                "user" => "owner_user",
                                "host" => "host_name",
                                other => other,
                            };
                            !filter.is_empty()
                                && !string(&row, field)
                                    .to_ascii_lowercase()
                                    .contains(&filter.to_ascii_lowercase())
                        }) {
                            continue;
                        }
                        let scoped_id = self.scoped(&name, string(&row, "public_id"));
                        let display_address = self.display_address(&name, &row);
                        let full_alias = format!(
                            "{}/{}#{}",
                            normalize_slug(string(&row, "owner_user")),
                            string(&row, "harness"),
                            string(&row, "listener_id").strip_prefix("c_").unwrap_or("")
                        );
                        let attachment_names =
                            crate::core::lease::attachments(&row, &self.host_id, now_ms())
                                .iter()
                                .map(|a| {
                                    format!(
                                        "{} {}",
                                        string(a, "display_name"),
                                        string(a, "host_id")
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join(" ");
                        let legacy_host =
                            if row["channels"].as_object().is_none_or(|c| c.is_empty()) {
                                format!("{} {}", string(&row, "host_name"), string(&row, "host_id"))
                            } else {
                                String::new()
                            };
                        let haystack = [
                            full_alias.as_str(),
                            string(&row, "title"),
                            string(&row, "about"),
                            string(&row, "repo"),
                            string(&row, "cwd"),
                            string(&row, "branch"),
                            string(&row, "owner_user"),
                            &legacy_host,
                            &attachment_names,
                            string(&row, "harness"),
                            string(&row, "public_id"),
                            &scoped_id,
                            &display_address,
                            string(&row, "listener_id"),
                        ]
                        .join(" ")
                        .to_lowercase();
                        if !query.split_whitespace().all(|token| {
                            let contains =
                                |field, value| string(&row, field).to_lowercase().contains(value);
                            if let Some(user) = token.strip_prefix('@').filter(|s| !s.is_empty()) {
                                contains("owner_user", user)
                            } else if let Some(user) =
                                token.strip_prefix("user:").filter(|s| !s.is_empty())
                            {
                                contains("owner_user", user)
                            } else if let Some(repo) =
                                token.strip_prefix("repo:").filter(|s| !s.is_empty())
                            {
                                contains("repo", repo)
                            } else if let Some(host) =
                                token.strip_prefix("host:").filter(|s| !s.is_empty())
                            {
                                host_matches(host)
                            } else if let Some(harness) =
                                token.strip_prefix("is:").filter(|s| !s.is_empty())
                            {
                                string(&row, "harness").to_lowercase() == harness
                            } else if let Some(harness) =
                                token.strip_prefix("harness:").filter(|s| !s.is_empty())
                            {
                                contains("harness", harness)
                            } else if let Some(id) =
                                token.strip_prefix('#').filter(|s| !s.is_empty())
                            {
                                contains("public_id", id)
                                    || scoped_id.to_lowercase().contains(id)
                                    || contains("listener_id", id)
                            } else {
                                haystack.contains(token)
                            }
                        }) {
                            continue;
                        }
                        rows.push(self.peer_dto(&name, &row));
                    }
                }
                Err(e) => warnings.push(json!({"hub":name,"message":e})),
            }
        }
        let mut seen = HashSet::new();
        rows.retain(|row| seen.insert(directory_identity(row)));
        rows.retain(include);
        rows.sort_by_key(|r| format!("{}:{}", string(r, "hub"), string(r, "listener_id")));
        let matched_count = rows.len();
        let online_count = matched_count;
        rows.truncate(limit as usize);
        Ok(
            json!({"ok":true,"listeners":rows,"warnings":warnings,"matched_count":matched_count,"online_count":online_count}),
        )
    }

    pub fn tool(&mut self, name: &str, args: &Value, meta: &Value) -> Result<Value, String> {
        if matches!(name, "cito_listen" | "cito_unlisten")
            && (args["harness"] == "human" || meta["expected_harness"] == "human")
        {
            return Err("FORBIDDEN: Agent tools cannot change Human device or inbox state".into());
        }
        validate_tool_input(name, args)?;
        if meta
            .get("expected_session_id")
            .is_some_and(|v| v.as_str().is_none_or(str::is_empty))
        {
            return Err("INVALID_INPUT: expected_session_id must be nonempty".into());
        }
        if meta.get("http_transport").is_some() || meta.get("http_client_pid").is_some() {
            return Err("INVALID_INPUT: Reserved caller metadata".into());
        }
        self.reload_slots()?;
        let hub = args["hub"]
            .as_str()
            .unwrap_or(&self.config.default_hub)
            .to_string();
        self.slot(&hub)?;
        match name {
            "cito_listen" => {
                let caller = self
                    .caller(meta, true)?
                    .ok_or("BINDING_REQUIRED: Caller required")?;
                if caller["harness"] == "human" {
                    return Err("FORBIDDEN: Agent tools cannot attach Human devices".into());
                }
                let mut args = args.clone();
                let raw = string(&caller, "title");
                let cwd = std::path::Path::new(string(&caller, "cwd"));
                let about = string(&args, "about");
                if meta["cli_cmd"] == "listen"
                    && !args["title"].is_string()
                    && self
                        .state
                        .custom_title(string(&caller, "harness"), string(&caller, "session_id"))?
                        .is_none()
                    && (raw.is_empty()
                        || cwd == self.config.home_dir
                        || cwd.file_name().and_then(|v| v.to_str()) == Some(raw))
                    && about != "CLI session"
                    && about.chars().count() <= 128
                {
                    args["title"] = json!(about);
                }
                let result = self.listen(&hub, &args, &caller)?;
                if let Some(title) = args["title"].as_str() {
                    self.rename_session(
                        string(&caller, "harness"),
                        string(&caller, "session_id"),
                        title,
                    )?;
                }
                Ok(result)
            }
            "cito_status" => {
                let caller = self
                    .caller(meta, false)?
                    .ok_or("BINDING_REQUIRED: Caller required")?;
                let row = self.state.active()?.into_iter().find(|r| {
                    r["hub"] == hub
                        && r["harness"] == caller["harness"]
                        && r["session_id"] == caller["session_id"]
                });
                let unread = if let Some(row) = &row {
                    self.state.db.query("SELECT COUNT(*) AS n FROM local_signals WHERE hub=? AND listener_id=? AND acknowledged_at_ms IS NULL", &[json!(hub), row["listener_id"].clone()])?.first().map(|r|r["n"].clone()).unwrap_or(json!(0))
                } else {
                    json!(0)
                };
                let mut hubs: Vec<_> = self.slots.iter().map(|(name, slot)|json!({"name":name,"status":wire_status(&slot.connection_status()),"is_default":name == &self.config.default_hub})).collect();
                hubs.sort_by(|a, b| string(a, "name").cmp(string(b, "name")));
                Ok(
                    json!({"ok":true,"registered":row.is_some(),"display_address":row.as_ref().map(|r| self.display_address(&hub,r)),"title":row.as_ref().map(|r|r["title"].clone()),"about":row.as_ref().map(|r|r["about"].clone()),"scope":row.as_ref().map(|r|r["scope"].clone()),"unread_count":unread,"hubs":hubs}),
                )
            }
            "cito_unlisten" => {
                let caller = self
                    .caller(meta, false)?
                    .ok_or("BINDING_REQUIRED: Caller required")?;
                if caller["harness"] == "human" {
                    return Err("FORBIDDEN: Agent tools cannot detach Human devices".into());
                }
                let hubs: Vec<String> = if args["hub"].is_string() {
                    vec![hub]
                } else {
                    self.slots.keys().cloned().collect()
                };
                let mut removed = 0;
                let mut detach_errors = Vec::new();
                for hub in hubs {
                    let id = self.ensure_agent_stop_row(
                        &hub,
                        string(&caller, "harness"),
                        string(&caller, "session_id"),
                    )?;
                    match self.pause_listener(&hub, &id) {
                        Ok(true) => removed += 1,
                        Ok(false) => {}
                        Err(error) => detach_errors.push(format!("{hub}: {error}")),
                    }
                }
                if !detach_errors.is_empty() {
                    return Err(format!("DETACH_PENDING: {}", detach_errors.join("; ")));
                }
                Ok(
                    json!({"ok":true,"status":if removed>0{"unlistened"}else{"not_listening"},"removed":removed}),
                )
            }
            "cito_find" => {
                let _ = self.caller(meta, false)?;
                self.find_listeners(args, |_| true)
            }
            "cito_send" => {
                let result = match self.send(&hub, args, meta) {
                    Err(e)
                        if e.starts_with("UNAUTHORIZED:")
                            || e.starts_with("AUTHORIZATION_VIOLATION:") =>
                    {
                        self.reload_slots()?;
                        self.send(&hub, args, meta)
                    }
                    result => result,
                };
                self.poll()?;
                result
            }
            "cito_inbox" => self.inbox(&hub, args, meta),
            _ => Err(format!("INVALID_INPUT: Unknown tool: {name}")),
        }
    }

    pub(super) fn send(&self, default: &str, args: &Value, meta: &Value) -> Result<Value, String> {
        let message = sanitize_message(
            args["message"]
                .as_str()
                .ok_or("INVALID_INPUT: message is required")?,
        )?;
        let raw = string(args, "to");
        if raw.is_empty() {
            return Err("INVALID_INPUT: to is required".into());
        }
        let (hub, to) = raw.split_once(':').unwrap_or((default, raw));
        if args["hub"].is_string() && hub != default {
            return Err("INVALID_INPUT: Conflicting hub prefixes".into());
        }
        let slot = self.slot(hub)?;
        let caller = self.caller(meta, false)?;
        let mut directory = slot.directory()?;
        let alias = crate::core::identity::readable_alias(to);
        let canonical_target = if to.starts_with('@') || alias.is_some() {
            None
        } else {
            Some(crate::core::identity::canonical_node_id(to)?)
        };
        let reply_to = if let Some(raw) = args["in_reply_to"].as_str() {
            if let Some((reply_hub, id)) = raw.split_once(':') {
                if reply_hub != hub {
                    return Err("INVALID_INPUT: Reply belongs to another Hub".into());
                }
                json!(id)
            } else {
                json!(raw)
            }
        } else {
            Value::Null
        };
        let handle = to.strip_prefix('@');
        let own_handle = handle.is_some_and(|handle| {
            handle == "self" || handle.eq_ignore_ascii_case(string(&slot.identity, "owner_user"))
        });
        if own_handle
            && !directory.iter().any(|row| {
                row["harness"] == "human"
                    && crate::core::auth::principal_ids_equal(
                        string(row, "principal_id"),
                        string(&slot.identity, "principal_id"),
                    )
            })
        {
            let disabled =
                crate::core::registration::local_node(&self.state, slot, "human", "default")?
                    .is_some_and(|row| row["status"] == "UNLISTENED");
            if !disabled {
                crate::core::registration::register_automatic_human(
                    &self.state,
                    slot,
                    &self.host_id,
                    &json!({"about":"Human receiver"}),
                    &json!({"harness":"human","session_id":"default","cwd":"","title":"Human"}),
                )?;
                directory = slot.directory()?;
            }
        }
        let exact_handle = handle.is_some_and(|handle| {
            directory.iter().any(|r| {
                r["harness"] == "human" && string(r, "owner_user").eq_ignore_ascii_case(handle)
            })
        });
        let mut matches: Vec<Value> = if let Some(handle) = handle {
            let user = if own_handle {
                string(&slot.identity, "owner_user")
            } else {
                handle
            };
            directory
                .into_iter()
                .filter(|row| {
                    row["harness"] == "human"
                        && if own_handle {
                            crate::core::auth::principal_ids_equal(
                                string(row, "principal_id"),
                                string(&slot.identity, "principal_id"),
                            )
                        } else {
                            string(row, "owner_user").eq_ignore_ascii_case(user)
                                || (!exact_handle
                                    && normalize_slug(string(row, "owner_user"))
                                        == normalize_slug(user))
                        }
                })
                .collect()
        } else if let Some((user, harness, prefix)) = alias {
            let mut found: Vec<_> = directory
                .into_iter()
                .filter(|row| {
                    matches!(
                        crate::core::lease::evaluate_node_lease(row, slot.server_now()),
                        "ACTIVE" | "DISCONNECTED_RETAINED"
                    ) && string(row, "listener_id")
                        .strip_prefix("c_")
                        .is_some_and(|s| s.starts_with(prefix))
                        && match harness {
                            Some(h) => {
                                string(row, "harness") == h
                                    && (string(row, "owner_user").eq_ignore_ascii_case(user)
                                        || normalize_slug(string(row, "owner_user")) == user)
                            }
                            None => {
                                string(row, "harness") == user
                                    || string(row, "owner_user").eq_ignore_ascii_case(user)
                                    || normalize_slug(string(row, "owner_user")) == user
                            }
                        }
                })
                .collect();
            // Bare harness aliases prefer the caller's principal; ambiguity within
            // that chosen scope still requires a full canonical ID.
            if harness.is_none()
                && found.iter().any(|row| {
                    string(row, "harness") == user
                        && crate::core::auth::principal_ids_equal(
                            string(row, "principal_id"),
                            string(&slot.identity, "principal_id"),
                        )
                })
            {
                found.retain(|row| {
                    string(row, "harness") == user
                        && crate::core::auth::principal_ids_equal(
                            string(row, "principal_id"),
                            string(&slot.identity, "principal_id"),
                        )
                });
            }
            if prefix.len() == 12
                && found
                    .iter()
                    .any(|r| string(r, "listener_id").strip_prefix("c_") == Some(prefix))
            {
                found.retain(|r| string(r, "listener_id").strip_prefix("c_") == Some(prefix));
            }
            found
        } else {
            directory
                .into_iter()
                .filter(|row| Some(string(row, "listener_id")) == canonical_target.as_deref())
                .collect()
        };
        if matches.is_empty() {
            if let Some(id) = canonical_target.as_deref() {
                match slot.kv_get(id) {
                    Ok(Some((row, _))) => matches.push(row),
                    Ok(None) => {}
                    Err(error) if error.starts_with("FORBIDDEN:") => {
                        return Err("FORBIDDEN_TARGET_SCOPE: Target is private".into())
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        if handle.is_some() && matches.len() > 1 {
            let now = slot.server_now();
            let is_live = |row: &Value| {
                matches!(
                    crate::core::lease::evaluate_node_lease(row, now),
                    "ACTIVE" | "DISCONNECTED_RETAINED"
                )
            };
            if matches.iter().any(is_live) {
                matches.retain(is_live);
            }
        }
        if matches.len() > 1 {
            if handle.is_none() {
                return Err("INVALID_INPUT: Ambiguous target; use full listener_id".into());
            }
            let principal = string(&matches[0], "principal_id");
            if matches.iter().any(|row| {
                !crate::core::auth::principal_ids_equal(principal, string(row, "principal_id"))
            }) {
                return Err("INVALID_INPUT: Ambiguous target; use full listener_id".into());
            }
            let preferred =
                crate::core::registration::local_node_id(&self.state, slot, "human", "default")?;
            matches.sort_by_key(|row| row["listener_id"] != preferred);
        }
        let target = matches
            .first()
            .ok_or("LISTENER_NOT_FOUND: Target listener does not exist")?;
        match crate::core::lease::evaluate_node_lease(target, slot.server_now()) {
            "UNLISTENED" => return Err("LISTENER_NOT_FOUND: Target listener was revoked".into()),
            "EXPIRED" => return Err("LISTENER_EXPIRED: Target listener expired".into()),
            _ => {}
        }
        let own_principal = crate::core::auth::principal_ids_equal(
            string(target, "principal_id"),
            string(&slot.identity, "principal_id"),
        );
        if !own_principal && target["scope"] != "workspace" {
            return Err("FORBIDDEN_TARGET_SCOPE: Target is private".into());
        }
        let canonical = string(target, "listener_id");
        if let Some(caller) = caller.as_ref().filter(|c| {
            !string(c, "harness").is_empty()
                && c["harness"] != "human"
                && !string(c, "session_id").is_empty()
        }) {
            if crate::core::registration::local_node(
                &self.state,
                slot,
                string(caller, "harness"),
                string(caller, "session_id"),
            )?
            .is_none()
            {
                let mut caller = caller.clone();
                let title = string(&caller, "title")
                    .trim()
                    .chars()
                    .take(500)
                    .collect::<String>();
                caller["title"] = json!(if title.is_empty() {
                    "Agent session"
                } else {
                    &title
                });
                self.listen(hub, &json!({"about":caller["title"]}), &caller)?;
            }
        }
        let from_id = caller
            .as_ref()
            .map(|caller| {
                crate::core::registration::local_node_id(
                    &self.state,
                    slot,
                    string(caller, "harness"),
                    string(caller, "session_id"),
                )
            })
            .transpose()?;
        if from_id.as_deref() == Some(canonical)
            || caller.as_ref().is_some_and(|caller| {
                own_principal
                    && caller["harness"] == target["harness"]
                    && caller["session_id"] == target["session_id"]
            })
        {
            return Err("SELF_SEND_FORBIDDEN: Cannot send to your own session".into());
        }
        let endpoint = format!(
            "{}|{}|{}|{}",
            string(&slot.identity, "principal_id"),
            self.host_id,
            caller
                .as_ref()
                .map(|c| string(c, "harness"))
                .unwrap_or("unbound"),
            caller
                .as_ref()
                .map(|c| string(c, "session_id"))
                .unwrap_or("default")
        );
        let key = args["idempotency_key"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(random_uuid_v4);
        let dedup = hash(&format!(
            "{endpoint}|{}|{canonical}|{key}",
            string(&slot.config, "url")
        ));
        let request_hash = hash(&format!("{message}|{}", reply_to));
        let now = now_ms();
        let reservation=self.state.db.transaction(|c|{
            c.execute("DELETE FROM local_sent_idempotency WHERE expires_at_ms<=?",&[json!(now)])?;
            if let Some(old)=c.query("SELECT * FROM local_sent_idempotency WHERE hub=? AND dedup_id=?",&[json!(hub),json!(dedup)])?.into_iter().next(){if old["request_hash"]!=request_hash{return Err("IDEMPOTENCY_KEY_CONFLICT: Different payload".into());}return Ok(old);}
            let id=format!("sig_{}",random_uuid_v4().replace('-',""));c.execute("INSERT INTO local_sent_idempotency(hub,dedup_id,request_hash,signal_id,to_listener_id,status,expires_at_iso,expires_at_ms) VALUES(?,?,?,?,?,'PENDING_PUBLISH',?,?)",&[json!(hub),json!(dedup),json!(request_hash),json!(id),json!(canonical),json!(iso(now+1_800_000)),json!(now+1_800_000)])?;
            c.query("SELECT * FROM local_sent_idempotency WHERE hub=? AND dedup_id=?",&[json!(hub),json!(dedup)])?.into_iter().next().ok_or("Sent reservation missing".into())
        })?;
        let mut signal_id = string(&reservation, "signal_id").to_owned();
        let mut seq = reservation["seq"].as_u64().unwrap_or(0);
        if reservation["status"] == "PENDING_PUBLISH" {
            let human = caller.as_ref().is_some_and(|c| c["harness"] == "human");
            let published = if human {
                // The Hub owns reply publication and source acknowledgment in one
                // transaction. Human sends without a source use the same publication path.
                slot.rpc("send_reply_from_human", &json!({"to":canonical,"message":message,"in_reply_to":reply_to,"reply_signal_id":signal_id,"msg_id":dedup,"host_id":self.host_id,"host_name":local_host_name()}))?
            } else {
                let local_sender = match from_id.as_deref() {
                    Some(id) => self.state.listener(hub, id)?,
                    None => None,
                };
                let authoritative_title = if let Some(caller) = &caller {
                    self.state
                        .custom_title(string(caller, "harness"), string(caller, "session_id"))?
                        .or_else(|| {
                            local_sender
                                .as_ref()
                                .and_then(|s| s["title"].as_str())
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned)
                        })
                        .unwrap_or_else(|| string(caller, "title").to_owned())
                } else {
                    String::new()
                };
                let signal = json!({"signal_id":signal_id,"to":canonical,"target_generation":target["binding_generation"],"sender_endpoint_key":endpoint,"from":{"principal_id":slot.identity["principal_id"],"user":slot.identity["owner_user"],"host_id":self.host_id,"host_name":local_host_name(),"harness":caller.as_ref().map(|c|c["harness"].clone()),"listener_id":from_id.as_deref().map(|id| local_sender.as_ref().and_then(|s|s["public_id"].as_str()).filter(|s|!s.is_empty()).map(str::to_string).unwrap_or_else(|| crate::core::identity::public_listener_id(string(&slot.identity,"owner_user"),id))),"title":authoritative_title,"about":local_sender.as_ref().map(|s|s["about"].clone())},"message":message,"in_reply_to":reply_to});
                slot.rpc("js_publish",&json!({"subject":format!("cito.signal.{canonical}"),"data_b64":STANDARD.encode(serde_json::to_vec(&signal).map_err(|e|e.to_string())?),"opts":{"msgID":dedup}}))?
            };
            seq = published["seq"].as_u64().unwrap_or(0);
            if human {
                signal_id = published["signal_id"]
                    .as_str()
                    .ok_or("Published reply ID missing")?
                    .to_owned();
            }
            // Commit local read state with the sent reservation. If this fails after
            // the Hub commits, retry the RPC with the same key and committed signal ID.
            self.state.db.transaction(|c| {
                if human && reply_to.is_string() {
                    c.execute("UPDATE local_signals SET acknowledged_at_ms=COALESCE(acknowledged_at_ms,?),state=CASE WHEN state='RECEIVED' THEN 'STORED_PASSIVE' ELSE state END WHERE hub=? AND listener_id=? AND signal_id=?", &[published["acked_at_ms"].clone(),json!(hub),json!(from_id),reply_to.clone()])?;
                }
                c.execute("UPDATE local_sent_idempotency SET signal_id=?,seq=?,status='wake_accepted' WHERE hub=? AND dedup_id=?",&[json!(signal_id),json!(seq),json!(hub),json!(dedup)])?;
                Ok(())
            })?;
        }
        Ok(
            json!({"ok":true,"signal_id":self.scoped(hub,&signal_id),"seq":seq,"to":self.scoped(hub,string(target,"public_id")),"status":"wake_accepted","held_reason":null,"expires_at":reservation["expires_at_iso"]}),
        )
    }
    fn inbox(&self, hub: &str, args: &Value, meta: &Value) -> Result<Value, String> {
        let caller = self
            .caller(meta, false)?
            .ok_or("BINDING_REQUIRED: Caller required")?;
        let limit = args["limit"].as_u64().unwrap_or(50);
        if !(1..=1000).contains(&limit) {
            return Err("INVALID_INPUT: limit must be 1..1000".into());
        }
        let mut bindings = Vec::new();
        let mut rows = Vec::new();
        for (name, slot) in &self.slots {
            if args["hub"].is_string() && name != hub {
                continue;
            }
            if slot.config["authMode"] == "oidc" && slot.bearer.is_none() {
                if args["hub"].is_string() {
                    return Err("UNAUTHORIZED: Hub requires authentication".into());
                }
                continue;
            }
            let id = crate::core::registration::local_node_id(
                &self.state,
                slot,
                string(&caller, "harness"),
                string(&caller, "session_id"),
            )?;
            rows.extend(self.state.db.query("SELECT * FROM local_signals WHERE hub=? AND listener_id=? AND acknowledged_at_ms IS NULL ORDER BY created_at,seq LIMIT ?",&[json!(name),json!(id),json!(limit)])?);
            bindings.push((name.clone(), id));
        }
        rows.sort_by_key(|r| {
            (
                string(r, "created_at").to_string(),
                string(r, "hub").to_string(),
                r["seq"].as_u64().unwrap_or(0),
            )
        });
        let page: Vec<Value> = rows
            .iter()
            .take(args["limit"].as_u64().unwrap_or(50) as usize)
            .cloned()
            .collect();
        let ack_all = args["ack_all"]
            .as_bool()
            .unwrap_or(args["ack_ids"].is_null());
        let mut selected: Vec<Value> = if ack_all { page.clone() } else { Vec::new() };
        for raw in args["ack_ids"].as_array().into_iter().flatten() {
            let raw = raw
                .as_str()
                .ok_or("INVALID_INPUT: Ack ID must be a string")?;
            let (scoped_hub, id) = match raw.split_once(':') {
                Some((h, id)) => {
                    if args["hub"].is_string() && h != hub {
                        return Err("INVALID_INPUT: Ack hub outside inbox".into());
                    }
                    (Some(h), id)
                }
                None => (None, raw),
            };
            let mut matches = Vec::new();
            for (name, node) in &bindings {
                if scoped_hub.is_some_and(|hub| hub != name) {
                    continue;
                }
                matches.extend(self.state.db.query("SELECT * FROM local_signals WHERE hub=? AND listener_id=? AND acknowledged_at_ms IS NULL AND (signal_id=? OR batch_id=?)",&[json!(name),json!(node),json!(id),json!(id)])?);
            }
            if scoped_hub.is_none()
                && matches
                    .iter()
                    .map(|r| string(r, "hub"))
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    > 1
            {
                return Err("INVALID_INPUT: Ambiguous ack ID; use hub prefix".into());
            }
            for row in matches {
                if !selected
                    .iter()
                    .any(|r| r["hub"] == row["hub"] && r["signal_id"] == row["signal_id"])
                {
                    selected.push(row);
                }
            }
        }
        for row in &selected {
            self.state.acknowledge(
                string(row, "hub"),
                string(row, "listener_id"),
                &[row["signal_id"].clone()],
                &format!("{}:{}", self.host_id, string(&caller, "harness")),
            )?;
        }
        let signals:Vec<Value>=page.iter().map(|row|json!({"hub":row["hub"],"signal_id":self.scoped(string(row,"hub"),string(row,"signal_id")),"seq":row["seq"],"from":{"user":row["from_user"],"host_name":row["from_host_name"],"harness":row["from_harness"],"listener_id":row["from_listener_id"],"title":row["from_title"],"about":row["from_about"]},"message":row["message"],"in_reply_to":row["in_reply_to"].as_str().map(|id|self.scoped(string(row,"hub"),id)),"created_at":row["created_at"],"held_reason":row["held_reason"]})).collect();
        self.flush_acks();
        Ok(
            json!({"ok":true,"signals":signals,"acknowledged_count":selected.len(),"replay_gap_detected":false,"replay_gap_range":null}),
        )
    }
    pub(super) fn flush_acks(&self) {
        for (hub, slot) in &self.slots {
            // Identity filtering precedes the page limit: retained rows for an
            // unavailable/replaced profile must not starve this profile's ACKs.
            let Ok(rows) = self.state.db.query("SELECT * FROM pending_hub_acks WHERE hub=? AND hub_url=? AND principal_id IN (?,?,?) ORDER BY acked_at_ms LIMIT 256",&[json!(hub),slot.config["url"].clone(),slot.identity["principal_id"].clone(),slot.identity["legacy_principal_id"].clone(),slot.identity["canonical_principal_id"].clone()]) else { continue; };
            for row in rows {
                if slot.rpc("signal_ack",&json!({"node_id":row["node_id"],"signal_ids":[row["signal_id"]],"channel_id":row["channel_id"]})).is_ok(){let _=self.state.db.execute("DELETE FROM pending_hub_acks WHERE hub=? AND node_id=? AND signal_id=?",&[row["hub"].clone(),row["node_id"].clone(),row["signal_id"].clone()]);}
            }
        }
    }
}
