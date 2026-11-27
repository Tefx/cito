use super::{
    projection::{directory_identity, wire_status},
    shutdown_requested, DaemonService, HubAccess,
};
use crate::core::{
    identity::{hash, iso, listener_id, local_host_name, normalize_slug, sanitize_message, string},
    registration::{distinct_label, is_generic_session_label},
};
use crate::plugins::{now_ms, random_uuid_v4};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

pub(super) const STALE_HUB: &str = "STALE_GENERATION: Request authority changed";

type DirectoryFlight = Arc<OnceLock<Result<Vec<Value>, String>>>;
#[derive(Default)]
pub(super) struct DirectoryFlights(Mutex<BTreeMap<(String, usize), DirectoryFlight>>);

impl DirectoryFlights {
    pub(super) fn refresh(
        &self,
        slot: &crate::core::transport::HubClient,
    ) -> Result<Vec<Value>, String> {
        let key = (slot.name.clone(), Arc::as_ptr(&slot.closed) as usize);
        let flight = self
            .0
            .lock()
            .map_err(|_| "Directory flights lock poisoned")?
            .entry(key.clone())
            .or_default()
            .clone();
        let result = flight.get_or_init(|| slot.directory());
        if let Ok(mut flights) = self.0.lock() {
            if flights
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, &flight))
            {
                flights.remove(&key);
            }
        }
        result.clone()
    }
}

#[derive(Clone, Default)]
pub(super) struct Stops {
    removed: usize,
    errors: Vec<String>,
}
impl Stops {
    fn apply(&mut self, access: &HubAccess, hubs: &[String], caller: &Value) {
        for hub in hubs {
            match access
                .ensure_agent_stop_row(hub, string(caller, "harness"), string(caller, "session_id"))
                .and_then(|id| access.retire_captured(hub, &id, "user"))
            {
                Ok(active) => self.removed += usize::from(active),
                Err(e) => self.errors.push(format!("{hub}: {e}")),
            }
        }
    }
    fn result(self) -> Result<Value, String> {
        if !self.errors.is_empty() {
            return Err(format!("DETACH_PENDING: {}", self.errors.join("; ")));
        }
        Ok(
            json!({"ok":true,"status":if self.removed>0 {"unlistened"} else {"not_listening"},"removed":self.removed}),
        )
    }
}

/// A batch action commits local stop intent first. Drain each remote Hub once,
/// outside service, before reporting its existing pending/error contract.
#[derive(Default)]
pub(super) struct Detaches(std::collections::BTreeSet<String>);
impl Detaches {
    pub(super) fn retire(
        &mut self,
        s: &DaemonService,
        hub: &str,
        id: &str,
        reason: &str,
    ) -> Result<bool, String> {
        let active = s.retire_local_locked(hub, id, reason, None, None)?;
        if s.slots.contains_key(hub) {
            self.0.insert(hub.into());
        }
        Ok(active)
    }
    pub(super) fn settle(self, value: Value, pending_is_error: bool) -> ToolCall {
        if self.0.is_empty() {
            ToolCall::Ready(value)
        } else {
            ToolCall::Drain {
                batch: self,
                value,
                pending_is_error,
            }
        }
    }
}

pub(super) enum ToolCall {
    Ready(Value),
    RegistrationEdit {
        action: Value,
        hubs: Vec<String>,
        ui_generation: Option<u64>,
        expected_profile: Option<Value>,
    },
    Drain {
        batch: Detaches,
        value: Value,
        pending_is_error: bool,
    },
    McpFind(Value),
    Find {
        args: Value,
        harness: Option<String>,
        mine: bool,
    },
    Listen {
        hub: String,
        args: Value,
        caller: Value,
    },
    Send {
        hub: String,
        args: Value,
        caller: Option<Value>,
    },
    HubControl {
        hub: String,
        action: Value,
    },
    Rename {
        caller: Value,
        title: String,
    },
    Switch {
        from: String,
        to: String,
        row: Value,
    },
    Unlisten {
        hubs: Vec<String>,
        caller: Value,
        stops: Stops,
    },
}
impl ToolCall {
    pub(super) fn execute(&self, snapshot: &HubAccess) -> Result<Value, String> {
        match self {
            Self::Ready(v) => Ok(v.clone()),
            Self::RegistrationEdit { .. } => {
                Err("STATE_ERROR: Registration edit requires a service owner".into())
            }
            Self::Drain {
                batch,
                value,
                pending_is_error,
            } => {
                let mut result = value.clone();
                let errors: Vec<_> = batch
                    .0
                    .iter()
                    .filter_map(|hub| {
                        snapshot
                            .slot(hub)
                            .and_then(|slot| {
                                DaemonService::reconcile_unlistened(
                                    &snapshot.state,
                                    slot,
                                    &snapshot.host_id,
                                )
                            })
                            .err()
                            .map(|e| format!("{hub}: {e}"))
                    })
                    .collect();
                if !errors.is_empty() {
                    let error = format!("DETACH_PENDING: {}", errors.join("; "));
                    if *pending_is_error {
                        return Err(error);
                    }
                    add_warning(&mut result, "warning", &error);
                    if result.get("detach_status").is_some() {
                        result["detach_status"] = json!("pending");
                        if let Some(w) = result["warnings"].as_array_mut() {
                            w.extend(errors.into_iter().map(Value::String));
                        }
                    }
                }
                Ok(result)
            }
            Self::McpFind(args) => snapshot.find_listeners(args, None),
            Self::Find {
                args,
                harness,
                mine,
            } => snapshot.find_listeners(args, Some((harness.as_deref(), *mine))),
            Self::Listen { hub, args, caller } => snapshot.listen(hub, args, caller),
            Self::Send { hub, args, caller } => snapshot.send_from(hub, args, caller.clone()),
            Self::Rename { caller, title } => snapshot.rename_session(
                string(caller, "harness"),
                string(caller, "session_id"),
                title,
            ),
            Self::HubControl { hub, action } => super::http_actions::hub_control(
                snapshot.slot(hub)?,
                &snapshot.state,
                &snapshot.host_id,
                action,
            ),
            Self::Switch { from, to, row } => snapshot.switch_binding(from, to, row),
            Self::Unlisten {
                hubs,
                caller,
                stops,
            } => {
                let mut stops = stops.clone();
                stops.apply(snapshot, hubs, caller);
                stops.result()
            }
        }
    }
    pub(super) fn degraded(
        &self,
        result: Result<Value, String>,
        reason: &str,
    ) -> Result<Value, String> {
        match result {
            Ok(mut value)
                if !matches!(self, Self::Find { .. } | Self::McpFind(_) | Self::Ready(_)) =>
            {
                value["committed"] = json!(true);
                let warning = if matches!(self, Self::Send { .. }) {
                    "maintenance_warning"
                } else {
                    "warning"
                };
                if !string(&value, warning).split("; ").any(|w| w == reason) {
                    add_warning(&mut value, warning, reason);
                }
                Ok(value)
            }
            Err(e) if e.starts_with("DETACH_PENDING:") => Err(e),
            _ => Err(reason.to_owned()),
        }
    }
    pub(super) fn current(&self, s: &DaemonService, access: &HubAccess) -> bool {
        !s.closing
            && !s.purging
            && !shutdown_requested()
            && s.slots
                .keys()
                .filter(|hub| self.targets(hub))
                .all(|hub| access.slots.contains_key(hub))
            && access
                .slots
                .iter()
                .filter(|(hub, _)| self.targets(hub))
                .all(|(hub, old)| {
                    !old.closed.load(Ordering::Acquire)
                        && s.slots
                            .get(hub)
                            .is_some_and(|live| Arc::ptr_eq(&old.closed, &live.closed))
                })
    }
    fn targets(&self, name: &str) -> bool {
        match self {
            Self::Ready(_) => false,
            Self::RegistrationEdit { hubs, .. } => hubs.iter().any(|hub| hub == name),
            Self::Drain { batch, .. } => batch.0.contains(name),
            Self::Find { args, .. } | Self::McpFind(args) => {
                args["hub"].as_str().is_none_or(|hub| hub == name)
            }
            Self::Listen { hub, .. } | Self::HubControl { hub, .. } => hub == name,
            Self::Rename { .. } => true,
            Self::Switch { from, to, .. } => from == name || to == name,
            Self::Send { hub, args, caller } => {
                if name == "local" && implicit_send(args, caller.as_ref()) {
                    return true;
                }
                string(args, "to")
                    .split_once(':')
                    .map_or(hub.as_str(), |(prefix, _)| prefix)
                    == name
            }
            Self::Unlisten { hubs, .. } => hubs.iter().any(|hub| hub == name),
        }
    }
}
pub(super) fn add_warning(value: &mut Value, key: &str, message: &str) {
    value[key] = json!(value[key]
        .as_str()
        .filter(|old| !old.is_empty())
        .map_or_else(|| message.to_owned(), |old| format!("{old}; {message}")));
}

pub(super) fn dispatch_tool<S: std::borrow::BorrowMut<DaemonService>>(
    service: &Mutex<S>,
    name: &str,
    args: &Value,
    meta: &Value,
) -> Result<Value, String> {
    if (name != "cito_find" && args["harness"] == "human")
        || string(meta, "expected_harness").trim() == "human"
    {
        return Err("FORBIDDEN: Agent tools cannot change Human device or inbox state".into());
    }
    validate_tool_input(name, args)?;
    if meta
        .get("expected_session_id")
        .is_some_and(|v| v.as_str().is_none_or(|s| s.trim().is_empty()))
    {
        return Err("INVALID_INPUT: expected_session_id must be nonempty".into());
    }
    if meta.get("http_transport").is_some() || meta.get("http_client_pid").is_some() {
        return Err("INVALID_INPUT: Reserved caller metadata".into());
    }
    let deadline = Instant::now() + Duration::from_millis(1500);
    let caller = if name == "cito_find" {
        crate::support::caller_pid_hint(meta, true)?;
        None
    } else {
        let access = {
            let mut guard = service.lock().map_err(|_| "Daemon lock poisoned")?;
            let s = (*guard).borrow_mut();
            if s.closing || s.purging || shutdown_requested() {
                return Err("Shutting down".into());
            }
            if name == "cito_status" {
                s.reload_slots()?;
                s.refresh_available_until(deadline)?;
            }
            s.access.clone()
        };
        access.caller_until(meta, name == "cito_listen", deadline)?
    };
    let (access, work) = {
        let mut guard = service.lock().map_err(|_| "Daemon lock poisoned")?;
        let s = (*guard).borrow_mut();
        if s.closing || s.purging || shutdown_requested() {
            return Err("Shutting down".into());
        }
        let work = s.prepare_tool(name, args, meta, caller)?;
        if let ToolCall::Ready(value) = work {
            return Ok(value);
        }
        (s.access.clone(), work)
    };
    run_captured(service, access, &work)
}

pub(super) fn run_captured<S: std::borrow::BorrowMut<DaemonService>>(
    service: &Mutex<S>,
    mut access: HubAccess,
    work: &ToolCall,
) -> Result<Value, String> {
    if let ToolCall::RegistrationEdit {
        action,
        hubs,
        ui_generation,
        expected_profile,
    } = work
    {
        if action["op"] == "setup_remove_app" {
            let id = action["harness"]
                .as_str()
                .ok_or("INVALID_INPUT: harness required")?;
            let plugin = access
                .plugins
                .find_by_id(id)
                .ok_or("UNSUPPORTED_HARNESS: Unknown harness")?;
            let removed = crate::setup::remove_plugin_setup(plugin);
            (*service.lock().map_err(|_| "Daemon lock poisoned")?)
                .borrow_mut()
                .invalidate_setup(id);
            super::http_actions::check_setup_actions(removed?)?;
        }
        // Take the per-hub guards in sorted order, outside service. Only local
        // intent/config commits run under both; detach I/O follows their release.
        let locks = hubs
            .iter()
            .map(|hub| access.state.registration_lock(hub))
            .collect::<Result<Vec<_>, _>>()?;
        let drain = 'commit: loop {
            let mut guards = Vec::with_capacity(locks.len());
            for lock in &locks {
                if let Some(guard) = super::try_registration(lock)? {
                    guards.push(guard);
                } else {
                    drop(guards);
                    drop(
                        lock.lock()
                            .map_err(|_| "STATE_ERROR: Registration lock poisoned")?,
                    );
                    continue 'commit;
                }
            }
            let mut guard = match service.try_lock() {
                Ok(guard) => guard,
                Err(std::sync::TryLockError::WouldBlock) => {
                    // A reload may be reaping an old worker. Never wait for it
                    // while owning a lock that worker needs to finish.
                    drop(guards);
                    drop(service.lock().map_err(|_| "Daemon lock poisoned")?);
                    continue;
                }
                Err(_) => return Err("Daemon lock poisoned".into()),
            };
            let s = (*guard).borrow_mut();
            s.validate_ui_generation(*ui_generation)?;
            if !work.current(s, &access) || s.registration_targets(action)? != *hubs {
                return Err(STALE_HUB.into());
            }
            let hub = action["hub"]
                .as_str()
                .unwrap_or(&s.config.default_hub)
                .to_owned();
            break s.apply_registration_edit(action, hub, expected_profile.as_ref())?;
        };
        let result = drain.execute(&access);
        let mut guard = match service.lock() {
            Ok(guard) => guard,
            Err(_) => return work.degraded(result, "Daemon lock poisoned during settlement"),
        };
        let value = result?;
        let s = (*guard).borrow_mut();
        if !work.current(s, &access) {
            return work.degraded(Ok(value), STALE_HUB);
        }
        let mut value = value;
        let projection = (|| {
            if action["op"] == "set_auto_listen" && action["enabled"] == true {
                s.auto_listen()?;
            }
            if matches!(
                string(action, "op"),
                "setup_disconnect_all" | "setup_uninstall_all"
            ) {
                Ok(())
            } else {
                s.reload_slots()
            }
        })();
        if let Err(error) = projection {
            value["committed"] = json!(true);
            add_warning(&mut value, "warning", &error);
        }
        return Ok(value);
    }
    let mut retried = false;
    loop {
        let result = work.execute(&access);
        let mut guard = match service.lock() {
            Ok(guard) => guard,
            Err(_) => return work.degraded(result, "Daemon lock poisoned during settlement"),
        };
        let s = (*guard).borrow_mut();
        let same = |s: &DaemonService| work.current(s, &access);
        let stale = |result| work.degraded(result, STALE_HUB);
        if !same(s) {
            return stale(result);
        }
        let unauthorized =
            |e: &str| e.starts_with("UNAUTHORIZED:") || e.starts_with("AUTHORIZATION_VIOLATION:");
        if !retried
            && match (work, &result) {
                (ToolCall::Send { .. }, Err(e)) => unauthorized(e),
                (ToolCall::Find { .. } | ToolCall::McpFind(_), Ok(value)) => {
                    value["warnings"].as_array().is_some_and(|ws| {
                        ws.iter().any(|w| {
                            access
                                .slots
                                .get(string(w, "hub"))
                                .is_some_and(|slot| slot.config["authMode"] == "oidc")
                                && unauthorized(string(w, "message"))
                        })
                    })
                }
                _ => false,
            }
        {
            retried = true;
            s.reload_slots()?;
            if !same(s) {
                return stale(result);
            }
            access = s.access.clone();
            continue;
        }
        let mut value = result?;
        if matches!(work, ToolCall::Send { .. }) {
            if let Err(error) = s.poll() {
                add_warning(&mut value, "maintenance_warning", &error);
            }
            if !same(s) {
                return stale(Ok(value));
            }
        }
        return Ok(value);
    }
}

/// Stream ACKs identify the submitted signal by sequence; Human replies also
/// return the canonical signal ID, which can differ on idempotent replay.
fn publication_receipt(result: &Value, submitted: Option<&Value>) -> Result<(String, u64), String> {
    let seq = result["seq"]
        .as_u64()
        .filter(|n| (1..=i64::MAX as u64).contains(n))
        .ok_or("INVALID_INPUT: Invalid publication sequence")?;
    let id = result
        .get("signal_id")
        .or(submitted)
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or("INVALID_INPUT: Published signal ID missing")?;
    Ok((id.to_owned(), seq))
}

#[cfg(test)]
#[test]
fn publication_receipts_require_sqlite_sequences_and_signal_identity() {
    let submitted = json!("original");
    for seq in [Value::Null, json!(0), json!(-1), json!(i64::MAX as u64 + 1)] {
        assert!(publication_receipt(&json!({"seq":seq,"signal_id":"sent"}), None).is_err());
    }
    let stream = json!({"seq":i64::MAX});
    assert_eq!(
        publication_receipt(&stream, Some(&submitted)).unwrap(),
        ("original".into(), i64::MAX as u64)
    );
    assert!(publication_receipt(&stream, None).is_err());
    for id in [Value::Null, json!(12), json!("  ")] {
        assert!(publication_receipt(&json!({"seq":1,"signal_id":id}), Some(&submitted)).is_err());
    }
    assert_eq!(
        publication_receipt(&json!({"seq":1,"signal_id":"canonical"}), None).unwrap(),
        ("canonical".into(), 1)
    );
}

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
    let scoped_id = |v: &Value| v.as_str().is_some_and(|s| (1..=161).contains(&s.len()));
    for (key, value) in map {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("INVALID_INPUT: Unknown argument: {key}"));
        }
        let valid = match key.as_str() {
            "limit" => value
                .as_u64()
                .is_some_and(|n| n > 0 && n <= if name == "cito_find" { 50 } else { 1000 }),
            "ack_all" => value.is_boolean(),
            "ack_ids" => value
                .as_array()
                .is_some_and(|a| a.len() <= 1000 && a.iter().all(scoped_id)),
            "in_reply_to" => scoped_id(value),
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
        ("idempotency_key", 128),
    ] {
        if let Some(value) = args[key].as_str() {
            if value.chars().count() > cap
                || matches!(key, "tag" | "idempotency_key") && value.trim().is_empty()
            {
                return Err(format!("INVALID_INPUT: Invalid {key} length"));
            }
        }
    }
    Ok(())
}

pub(super) fn validate_agent_session(harness: &str, session: &str) -> Result<(), String> {
    if !crate::plugins::validate_harness_id(harness)
        || harness == "human"
        || session.trim().is_empty()
    {
        return Err("INVALID_INPUT: An agent session is required".into());
    }
    Ok(())
}

pub(super) fn tools_list() -> Value {
    let mut tools = json!({"tools":[
     {"name":"cito_status","description":"Use cito_status to check this session's Cito control plane registration, address, connected hubs, and unread signal count.","inputSchema":{"type":"object","properties":{"hub":{"type":"string"}}}},
     {"name":"cito_listen","description":"Use cito_listen to register this session as a peer node on the Cito control plane so it can receive wake signals.","inputSchema":{"type":"object","properties":{"about":{"type":"string","minLength":1,"maxLength":500},"title":{"type":"string","minLength":1,"maxLength":128},"tag":{"type":"string","maxLength":64},"hub":{"type":"string"},"scope":{"type":"string","enum":["self","workspace"]}},"required":["about"]}},
     {"name":"cito_unlisten","description":"Use cito_unlisten to stop this peer node from receiving wake signals on one or all Cito control plane hubs.","inputSchema":{"type":"object","properties":{"hub":{"type":"string"}}}},
     {"name":"cito_find","description":"Use cito_find to discover a peer node on the Cito control plane and get its address for cito_send.","inputSchema":{"type":"object","properties":{"hub":{"type":"string"},"query":{"type":"string","maxLength":200},"user":{"type":"string","maxLength":64},"host":{"type":"string","maxLength":128},"harness":{"type":"string","pattern":"^[a-z0-9][a-z0-9_-]{0,31}$"},"repo":{"type":"string","maxLength":200},"limit":{"type":"integer","minimum":1,"maximum":50}}}},
     {"name":"cito_send","description":"Use cito_send to deliver a wake signal of 1..65,536 Unicode code points to a peer node on the Cito control plane. Prefer external channels for long debates or large artifacts.","inputSchema":{"type":"object","properties":{"to":{"type":"string","pattern":"^(?:[A-Za-z0-9][A-Za-z0-9_-]{0,31}:)?(?:c_(?:[a-f0-9]{16}|[a-z2-7]{12})|[a-z0-9]+(?:-[a-z0-9]+)*-(?:[a-f0-9]{16}|[a-z2-7]{12})|@[a-zA-Z0-9._-]{1,64}|[a-zA-Z0-9._-]{1,64}(?:/[a-z0-9][a-z0-9_-]{0,31})?#[a-z0-9]{1,16})$"},"message":{"type":"string","minLength":1,"maxLength":65536},"hub":{"type":"string"},"in_reply_to":{"type":"string","minLength":1,"maxLength":161},"idempotency_key":{"type":"string","minLength":1,"maxLength":128}},"required":["to","message"]}},
     {"name":"cito_inbox","description":"Use cito_inbox to read and acknowledge pending wake signals for this peer node on the Cito control plane. Delivered wake cards are already auto-acknowledged.","inputSchema":{"type":"object","properties":{"hub":{"type":"string"},"ack_all":{"type":"boolean"},"ack_ids":{"type":"array","maxItems":1000,"items":{"type":"string","minLength":1,"maxLength":161}},"limit":{"type":"integer","minimum":1,"maximum":1000}}}}
    ]});
    if let Some(list) = tools["tools"].as_array_mut() {
        for tool in list {
            tool["inputSchema"]["additionalProperties"] = json!(false);
        }
    }
    tools
}

impl HubAccess {
    pub(super) fn caller(&self, meta: &Value, required: bool) -> Result<Option<Value>, String> {
        self.caller_until(meta, required, Instant::now() + Duration::from_millis(1500))
    }

    fn caller_until(
        &self,
        meta: &Value,
        required: bool,
        deadline: Instant,
    ) -> Result<Option<Value>, String> {
        let pid_hint = crate::support::caller_pid_hint(meta, true)?.map(u64::from);
        let requested_pid = pid_hint.or_else(|| meta["_http_client"].as_u64());
        let explicit_pid = pid_hint.is_some();
        let expected = string(meta, "expected_harness").trim();
        if expected == "human" {
            return Err("FORBIDDEN: MCP callers must be agent sessions".into());
        }
        if !required
            && requested_pid.is_none()
            && expected.is_empty()
            && string(meta, "expected_session_id").is_empty()
        {
            return Ok(None);
        }
        if !expected.is_empty()
            && !self
                .plugins
                .agent_plugins()
                .iter()
                .any(|plugin| plugin.id == expected)
        {
            return Err("UNSUPPORTED_HARNESS: Unknown harness".into());
        }
        let mut failures = Vec::new();
        let mut plugins = self.plugins.agent_plugins();
        if expected.is_empty() && explicit_pid {
            plugins.sort_by_key(|plugin| !plugin.pid_bound);
        }
        for plugin in plugins {
            if !expected.is_empty() && plugin.id != expected {
                continue;
            }
            if Instant::now() >= deadline {
                return Err("TOOL_TIMEOUT: Caller resolution exceeded 1500ms budget".into());
            }
            let mut request = json!({"op":"resolve_caller","meta":meta,"headers":meta["headers"],"cwd":meta["cwd"],"http_transport":true,"http_client_pid":requested_pid,"require_open":required,"explicit_session_id":if expected == plugin.id { meta["expected_session_id"].clone() } else { Value::Null },"cli_cmd":meta["cli_cmd"],"state_dir":self.config.state_dir,"env":{}});
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
            match crate::plugins::plugin_request_with_progress(
                plugin,
                &request,
                deadline.saturating_duration_since(Instant::now()),
                |_| {
                    if shutdown_requested() {
                        Err("Caller discovery cancelled".into())
                    } else {
                        Ok(())
                    }
                },
            ) {
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
                            cache.retain(|pid, _| {
                                u32::try_from(*pid).is_ok_and(crate::support::alive)
                            });
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
        if Instant::now() >= deadline {
            return Err("TOOL_TIMEOUT: Caller resolution exceeded 1500ms budget".into());
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
                .unwrap_or_else(|| "BINDING_REQUIRED: No caller socket bridge found".into()));
        }
        if required {
            return Err("BINDING_REQUIRED: No caller socket bridge found".into());
        }
        Ok(None)
    }
}
impl HubAccess {
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
        Ok(self.propagate_session_title(harness,session,title).unwrap_or_else(|error|json!({"ok":true,"committed":true,"title":title,"warning":error,"warnings":[{"message":error}]})))
    }

    fn propagate_session_title(
        &self,
        harness: &str,
        session: &str,
        title: &str,
    ) -> Result<Value, String> {
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
        let warning = (!warnings.is_empty()).then(|| {
            warnings
                .iter()
                .map(|w| string(w, "message"))
                .collect::<Vec<_>>()
                .join("; ")
        });
        Ok(json!({"ok":true,"title":title,"warning":warning,"warnings":warnings}))
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
        let mut result = crate::core::registration::register_endpoint(
            &self.state,
            self.slot(hub)?,
            &self.host_id,
            args,
            caller,
        )?;
        // The new remote/local binding commits before the previous endpoint is revoked.
        let mut warnings = Vec::new();
        if caller["endpoint_pid"].is_number() {
            if let Err(error) =
                DaemonService::reconcile_unlistened(&self.state, self.slot(hub)?, &self.host_id)
            {
                warnings.push(error);
            }
        }
        if caller["harness"] != "human" && result["maintenance_warning"].is_null() {
            if let Some(title) = args["title"].as_str() {
                let propagated = self.propagate_session_title(
                    string(caller, "harness"),
                    string(caller, "session_id"),
                    title,
                );
                if propagated
                    .as_ref()
                    .ok()
                    .is_none_or(|value| value["warning"].is_string())
                {
                    let warning = "Listening committed; session title maintenance is incomplete";
                    result["maintenance_warning"] = json!(warning);
                    warnings.push(warning.to_owned());
                }
            }
        }
        if !warnings.is_empty() {
            if let Some(previous) = result["warning"].as_str() {
                warnings.insert(0, previous.to_owned());
            }
            result["committed"] = json!(true);
            result["warning"] = json!(warnings.join("; "));
            result["warnings"] = json!(warnings);
        }
        // Registration commits the Hub-scoped PID and transition timestamp together.
        // Do not rewrite binding metadata after releasing registration authority.
        let slot = self.slot(hub)?;
        let _ = slot.directory();
        result["display_address"] = json!(crate::core::identity::display_address(
            hub,
            &self.config.default_hub,
            &json!({"owner_user":result["user"],"harness":result["harness"],"listener_id":result["canonical_listener_id"]}),
            &super::projection::address_peers(&self.state, slot),
        ));
        result["listener_id"] = json!(self.scoped(hub, string(&result, "listener_id")));
        Ok(result)
    }

    pub(super) fn session_hubs(&self, harness: &str, session: &str) -> Result<Vec<String>, String> {
        let mut hubs: Vec<_> = self.slots.keys().cloned().collect();
        hubs.extend(self.state.db.query("SELECT DISTINCT hub FROM local_listeners WHERE harness=? AND session_id=? AND status!='FENCED_INACTIVE'", &[json!(harness),json!(session)])?.iter().map(|row| string(row,"hub").to_owned()));
        hubs.sort();
        hubs.dedup();
        Ok(hubs)
    }
    /// Resolve retirement from live identity or the durable offline binding.
    pub(super) fn session_node_id(
        &self,
        hub: &str,
        harness: &str,
        session: &str,
    ) -> Result<String, String> {
        match self.slot(hub) {
            Ok(slot) => super::super::registration::local_node_id(&self.state, slot, harness, session),
            Err(error) => self.state.db.query("SELECT listener_id FROM local_listeners WHERE hub=? AND harness=? AND session_id=? AND status!='FENCED_INACTIVE' ORDER BY binding_generation DESC LIMIT 1", &[json!(hub),json!(harness),json!(session)])?.first().map(|row| string(row,"listener_id").to_owned()).ok_or(error),
        }
    }

    /// Stop intent exists even if this session has never been enrolled.
    pub(super) fn ensure_agent_stop_row(
        &self,
        hub: &str,
        harness: &str,
        session: &str,
    ) -> Result<String, String> {
        validate_agent_session(harness, session)?;
        match self.slot(hub) {
            Err(_) => return self.session_node_id(hub, harness, session),
            Ok(slot) if slot.closed.load(Ordering::Acquire) => return Err(STALE_HUB.into()),
            _ => {}
        }
        let lock = self.state.registration_lock(hub)?;
        let _guard = lock
            .lock()
            .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
        self.ensure_agent_stop_row_locked(hub, harness, session)
    }
    pub(super) fn ensure_agent_stop_row_locked(
        &self,
        hub: &str,
        harness: &str,
        session: &str,
    ) -> Result<String, String> {
        validate_agent_session(harness, session)?;
        let Ok(slot) = self.slot(hub) else {
            return self.session_node_id(hub, harness, session);
        };
        let id = super::super::registration::local_node_id(&self.state, slot, harness, session)?;
        self.state.db.transaction(|db| {
        if slot.closed.load(Ordering::Acquire) { return Err(STALE_HUB.into()); }
        // The replacement URL may have the same deterministic node ID. Old
        // signals remain tied to their recorded authority, not the new Stop row.
        db.execute("DELETE FROM local_listeners WHERE hub=? AND listener_id=? AND status='FENCED_INACTIVE'", &[json!(hub),json!(id)])?;
        db.execute("INSERT OR IGNORE INTO local_listeners(hub,listener_id,public_id,principal_id,owner_user,host_id,host_name,harness,session_id,cwd,title,about,scope,status,user_stopped,retirement_reason,hub_url) VALUES(?,?,?,?,?,?,?,?,?,'','','','self','UNLISTENED',1,'user',?)", &[json!(hub),json!(id),json!(crate::core::identity::public_listener_id(string(&slot.identity,"owner_user"),&id)),slot.identity["principal_id"].clone(),slot.identity["owner_user"].clone(),json!(self.host_id),json!(local_host_name()),json!(harness),json!(session),slot.config["url"].clone()])?;
        Ok(()) })?;
        Ok(id)
    }
}
impl HubAccess {
    pub(super) fn retire_captured(
        &self,
        hub: &str,
        id: &str,
        reason: &str,
    ) -> Result<bool, String> {
        self.retire_binding(
            hub,
            id,
            reason,
            self.slots.get(hub).map(|slot| slot.closed.as_ref()),
            None,
        )
    }
    pub(super) fn retire_binding(
        &self,
        hub: &str,
        id: &str,
        reason: &str,
        replaced: Option<&std::sync::atomic::AtomicBool>,
        expected: Option<&Value>,
    ) -> Result<bool, String> {
        let lock = self.state.registration_lock(hub)?;
        let guard = self
            .slots
            .get(hub)
            .filter(|s| !s.closed.load(Ordering::Acquire))
            .map(|_| {
                lock.lock()
                    .map_err(|_| "STATE_ERROR: Registration lock poisoned")
            })
            .transpose()?;
        let active = self.retire_local_locked(hub, id, reason, replaced, expected)?;
        drop(guard);
        if let Some(client) = self.slots.get(hub) {
            if client.closed.load(Ordering::Acquire) {
                return Err("DETACH_PENDING: Hub changed before detach".into());
            }
            DaemonService::reconcile_unlistened(&self.state, client, &self.host_id)?;
        }
        Ok(active)
    }
    fn retire_local_locked(
        &self,
        hub: &str,
        id: &str,
        reason: &str,
        replaced: Option<&std::sync::atomic::AtomicBool>,
        expected: Option<&Value>,
    ) -> Result<bool, String> {
        let client = self.slots.get(hub);
        let row = self.state.listener(hub, id)?;
        if row
            .as_ref()
            .is_some_and(|r| r["status"] == "FENCED_INACTIVE")
        {
            return Err(STALE_HUB.into());
        }
        let hub_url = if let Some(url) = row
            .as_ref()
            .and_then(|r| r["hub_url"].as_str())
            .filter(|s| !s.is_empty())
        {
            Some(url.to_owned())
        } else if let Some(client) = client {
            Some(string(&client.config, "url").to_owned())
        } else {
            // Disabled Hubs have no live slot; their normalized configuration
            // still identifies the only target a deferred detach may affect.
            let raw = self.raw_config()?;
            let (_, hubs, _) =
                crate::plugins::try_normalize_existing_config(Some(&raw.to_string()))?;
            hubs.get(hub).map(|config| string(config, "url").to_owned())
        };
        let active = row.as_ref().is_some_and(|row| row["status"] == "ACTIVE");
        self.state.db.transaction(|c| {
            if replaced.is_some_and(|closed| closed.load(Ordering::Acquire)) { return Err(STALE_HUB.into()); }
            if let Some(old)=expected {
                if c.query("SELECT 1 FROM local_listeners WHERE hub=? AND listener_id=? AND binding_generation=? AND owner_token=? AND status='ACTIVE'",&[json!(hub),json!(id),old["binding_generation"].clone(),old["owner_token"].clone()])?.is_empty(){return Err("STALE_GENERATION: Source binding changed before retirement".into());}
            }
            c.execute("DELETE FROM wake_retries WHERE hub=? AND listener_id=?", &[json!(hub),json!(id)])?;
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
        // A failed Hub write leaves durable pending work; poll/reconnect retries it.
        if client.is_none()
            && !self
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
        &self,
        args: &Value,
        ui_filters: Option<(Option<&str>, bool)>,
    ) -> Result<Value, String> {
        let hub = args["hub"]
            .as_str()
            .unwrap_or(&self.config.default_hub)
            .to_string();
        let limit = args["limit"].as_u64().unwrap_or(20);
        if !(1..=50).contains(&limit) {
            return Err("INVALID_INPUT: limit must be 1..50".into());
        }
        let (order, _, _) = self.hub_projection_config();
        let ranks: BTreeMap<_, _> = order
            .iter()
            .enumerate()
            .map(|(rank, name)| (name.as_str(), rank))
            .collect();
        let hub_rank = |name: &str| ranks.get(name).copied().unwrap_or(usize::MAX);
        let default_priority = |name: &str| ui_filters.is_none() && name != self.config.default_hub;
        let query = string(args, "query").to_lowercase();
        let mut groups = BTreeMap::new();
        let mut warnings = Vec::new();
        let mut snapshots = thread::scope(|scope| {
            let workers: Vec<_> = self
                .slots
                .iter()
                .filter(|(name, _)| !args["hub"].is_string() || *name == &hub)
                .map(|(name, slot)| {
                    let worker = scope.spawn(move || self.directory_flights.refresh(slot));
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
                            Err("HUB_UNAVAILABLE: Directory worker failed".into())
                        }),
                    )
                })
                .collect::<Vec<_>>()
        });
        // MCP deduplication keeps the first row, so apply the same Hub priority
        // before grouping as after it.
        snapshots.sort_by_key(|(name, _, _)| (default_priority(name), hub_rank(name)));
        for (name, slot, snapshot) in snapshots {
            match snapshot {
                Ok(directory) => {
                    let peers = super::projection::address_peers(&self.state, &slot);
                    let sample = super::projection::HubSample::capture(&slot, directory);
                    for row in &sample.directory {
                        let availability = crate::core::lease::evaluate_node_lease(row, now_ms());
                        if !matches!(availability, "ACTIVE" | "DISCONNECTED_RETAINED")
                            || (row["scope"] == "self"
                                && !crate::core::auth::principal_ids_equal(
                                    string(row, "principal_id"),
                                    string(&slot.identity, "principal_id"),
                                ))
                        {
                            continue;
                        }
                        let host_matches = |term: &str| {
                            let term = term.to_lowercase();
                            (row["channels"].as_object().is_none_or(|c| c.is_empty())
                                && (string(row, "host_name").to_lowercase().contains(&term)
                                    || string(row, "host_id").to_lowercase().contains(&term)))
                                || row["channels"]
                                    .as_object()
                                    .into_iter()
                                    .flat_map(|channels| channels.values())
                                    .filter(|channel| {
                                        matches!(
                                            crate::core::lease::attachment_availability(
                                                channel,
                                                crate::core::lease::registration_now(row, now_ms())
                                            ),
                                            "ACTIVE" | "DISCONNECTED_RETAINED"
                                        )
                                    })
                                    .any(|channel| {
                                        ["display_name", "host_name", "host_id"].iter().any(|key| {
                                            string(channel, key).to_lowercase().contains(&term)
                                        })
                                    })
                        };
                        if ["user", "harness", "repo", "host"].iter().any(|key| {
                            let filter = string(args, key);
                            if *key == "host" {
                                return !filter.is_empty() && !host_matches(filter);
                            }
                            if *key == "harness" {
                                return !filter.is_empty() && string(row, "harness") != filter;
                            }
                            let field = if *key == "user" { "owner_user" } else { key };
                            !filter.is_empty()
                                && !string(row, field)
                                    .to_ascii_lowercase()
                                    .contains(&filter.to_ascii_lowercase())
                        }) {
                            continue;
                        }
                        let scoped_id = self.scoped(&name, string(row, "public_id"));
                        let display_address = crate::core::identity::display_address(
                            &name,
                            &self.config.default_hub,
                            row,
                            &peers,
                        );
                        let full_alias = format!(
                            "{}/{}#{}",
                            normalize_slug(string(row, "owner_user")),
                            string(row, "harness"),
                            string(row, "listener_id").strip_prefix("c_").unwrap_or("")
                        );
                        let attachment_names =
                            crate::core::lease::attachments(row, &self.host_id, now_ms())
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
                                format!("{} {}", string(row, "host_name"), string(row, "host_id"))
                            } else {
                                String::new()
                            };
                        let haystack = [
                            full_alias.as_str(),
                            string(row, "title"),
                            string(row, "about"),
                            string(row, "repo"),
                            string(row, "cwd"),
                            string(row, "branch"),
                            string(row, "owner_user"),
                            &legacy_host,
                            &attachment_names,
                            string(row, "harness"),
                            string(row, "public_id"),
                            &scoped_id,
                            &display_address,
                            string(row, "listener_id"),
                        ]
                        .join(" ")
                        .to_lowercase();
                        if !query.split_whitespace().all(|token| {
                            let contains =
                                |field, value| string(row, field).to_lowercase().contains(value);
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
                                string(row, "harness").to_lowercase() == harness
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
                        let dto = self.peer_dto(&name, row, &display_address, &sample);
                        if let Some((harness, mine)) = ui_filters {
                            if harness.is_none_or(|h| dto["harness"] == h)
                                && (!mine || dto["is_self_principal"] == true)
                            {
                                groups.entry(directory_identity(&dto)).or_insert((
                                    dto,
                                    Vec::new(),
                                    availability == "ACTIVE",
                                ));
                            }
                        } else {
                            let (_, hubs, online) = groups
                                .entry((String::new(), string(row, "listener_id").into()))
                                .or_insert_with(|| (dto, Vec::new(), false));
                            *online |= availability == "ACTIVE";
                            if hubs.last() != Some(&name) {
                                hubs.push(name.clone());
                            }
                        }
                    }
                }
                Err(e) => warnings.push(json!({"hub":name,"message":e})),
            }
        }
        let online_count = groups.values().filter(|(_, _, online)| *online).count();
        let mut rows: Vec<_> = groups
            .into_values()
            .map(|(mut row, hubs, _)| {
                if ui_filters.is_none() {
                    if args["hub"].is_null() {
                        row["hubs"] = json!(hubs);
                    }
                    if let Some(fields) = row.as_object_mut() {
                        fields.retain(|key, _| {
                            !key.starts_with("can_") && key != "human_can_set_scope"
                        });
                    }
                }
                row
            })
            .collect();
        rows.sort_by_key(|r| {
            (
                default_priority(string(r, "hub")),
                hub_rank(string(r, "hub")),
                directory_identity(r),
            )
        });
        let matched_count = rows.len();
        rows.truncate(limit as usize);
        Ok(
            json!({"ok":true,"listeners":rows,"warnings":warnings,"matched_count":matched_count,"online_count":online_count}),
        )
    }
}
impl DaemonService {
    pub fn tool(&mut self, name: &str, args: &Value, meta: &Value) -> Result<Value, String> {
        dispatch_tool(&Mutex::new(self), name, args, meta)
    }

    fn prepare_tool(
        &mut self,
        name: &str,
        args: &Value,
        meta: &Value,
        caller: Option<Value>,
    ) -> Result<ToolCall, String> {
        self.reload_slots()?;
        let hub = args["hub"]
            .as_str()
            .unwrap_or(&self.config.default_hub)
            .to_string();
        if name != "cito_unlisten" {
            self.slot(&hub)?;
        }
        match name {
            "cito_listen" => {
                let caller = caller.ok_or("BINDING_REQUIRED: Caller required")?;
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
                Ok(ToolCall::Listen { hub, args, caller })
            }
            "cito_status" => {
                let caller = caller.ok_or("BINDING_REQUIRED: Caller required")?;
                let row = self.state.active()?.into_iter().find(|r| {
                    r["hub"] == hub
                        && r["harness"] == caller["harness"]
                        && r["session_id"] == caller["session_id"]
                });
                let unread = if let Some(row) = &row {
                    json!(self
                        .state
                        .unread_count(&hub, string(row, "listener_id"), false)?)
                } else {
                    json!(0)
                };
                let mut hubs: Vec<_> = self.slots.iter().map(|(name, slot)|json!({"name":name,"status":wire_status(&slot.connection_status()),"is_default":name == &self.config.default_hub})).collect();
                hubs.sort_by(|a, b| string(a, "name").cmp(string(b, "name")));
                Ok(ToolCall::Ready(
                    json!({"ok":true,"registered":row.is_some(),"display_address":row.as_ref().map(|r| self.display_address(&hub,r)),"title":row.as_ref().map(|r|r["title"].clone()),"about":row.as_ref().map(|r|r["about"].clone()),"scope":row.as_ref().map(|r|r["scope"].clone()),"unread_count":unread,"hubs":hubs}),
                ))
            }
            "cito_unlisten" => {
                let caller = caller.ok_or("BINDING_REQUIRED: Caller required")?;
                if caller["harness"] == "human" {
                    return Err("FORBIDDEN: Agent tools cannot detach Human devices".into());
                }
                let hubs: Vec<String> = if args["hub"].is_string() {
                    vec![hub]
                } else {
                    self.session_hubs(string(&caller, "harness"), string(&caller, "session_id"))?
                };
                // A disabled Hub has no live generation to capture. Settle its
                // local intent here while it cannot be re-enabled.
                let (live, offline): (Vec<_>, _) =
                    hubs.into_iter().partition(|h| self.slots.contains_key(h));
                let mut stops = Stops::default();
                stops.apply(&self.access, &offline, &caller);
                Ok(ToolCall::Unlisten {
                    hubs: live,
                    caller,
                    stops,
                })
            }
            "cito_find" => Ok(ToolCall::McpFind(args.clone())),
            "cito_send" => Ok(ToolCall::Send {
                hub,
                args: args.clone(),
                caller,
            }),
            "cito_inbox" => self
                .inbox(
                    &hub,
                    args,
                    &caller.ok_or("BINDING_REQUIRED: Caller required")?,
                )
                .map(ToolCall::Ready),
            _ => Err(format!("INVALID_INPUT: Unknown tool: {name}")),
        }
    }

    #[cfg(test)]
    pub(super) fn send(&self, default: &str, args: &Value, meta: &Value) -> Result<Value, String> {
        let caller = self.caller(meta, false)?;
        self.send_from(default, args, caller)
    }
}
fn implicit_send(args: &Value, caller: Option<&Value>) -> bool {
    args["hub"].is_null()
        && !string(args, "to").contains(':')
        && caller.is_none_or(|c| c["harness"] != "human")
}

// Persisted and wire dedup IDs include the resolved listener. On a miss (or an
// offline alias), probe using stored targets rather than consulting the directory.
fn sent_reservation(
    db: &crate::core::database::Connection,
    hub: &str,
    target: Option<&str>,
    now: u64,
    dedup_id: impl Fn(&str) -> String,
) -> Result<Option<Value>, String> {
    let find = |id: &str| {
        db.query(
            "SELECT * FROM local_sent_idempotency WHERE hub=? AND dedup_id=? AND expires_at_ms>?",
            &[json!(hub), json!(dedup_id(id)), json!(now)],
        )
        .map(|mut rows| rows.pop())
    };
    if let Some(id) = target {
        if let Some(old) = find(id)? {
            return Ok(Some(old));
        }
    }
    let mut old = None;
    db.query_while(
        "SELECT DISTINCT to_listener_id FROM local_sent_idempotency WHERE hub=? AND expires_at_ms>? AND to_listener_id IS NOT ?",
        &[json!(hub), json!(now), json!(target)],
        |row| {
            old = find(string(&row, "to_listener_id"))?;
            if old.is_some() && target.is_some() {
                return Err("IDEMPOTENCY_KEY_CONFLICT: Different target".into());
            }
            Ok(old.is_none())
        },
    )?;
    Ok(old)
}

impl HubAccess {
    fn prepare_send(
        &self,
        hub: &str,
        to: &str,
        reply_hub: Option<&str>,
        caller: Option<&Value>,
        fallback: Option<&str>,
        reservation: Option<&Value>,
    ) -> Result<(Value, Option<String>), String> {
        let slot = self.slot(hub)?;
        let mut directory = slot.directory()?;
        if fallback.is_some() {
            directory.retain(|row| {
                crate::core::lease::evaluate_node_lease(row, slot.server_now()) == "ACTIVE"
            });
        }
        let alias = crate::core::identity::readable_alias(to);
        let canonical_target = if to.starts_with('@') || alias.is_some() {
            None
        } else {
            Some(crate::core::identity::canonical_node_id(to)?)
        };
        if reply_hub.is_some_and(|name| name != hub) {
            return Err("INVALID_INPUT: Reply belongs to another Hub".into());
        }
        let handle = to.strip_prefix('@');
        let own_handle = handle.is_some_and(|handle| {
            handle == "self" || handle.eq_ignore_ascii_case(string(&slot.identity, "owner_user"))
        });
        if fallback.is_none()
            && reservation.is_none()
            && own_handle
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
            if let Some(error) = fallback {
                return Err(error.to_owned());
            }
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
        let target = matches.into_iter().next();
        if reservation.is_some_and(|old| {
            target
                .as_ref()
                .is_none_or(|row| row["listener_id"] != old["to_listener_id"])
        }) {
            return Err("IDEMPOTENCY_KEY_CONFLICT: Different target".into());
        }
        let target = target.ok_or("LISTENER_NOT_FOUND: Target listener does not exist")?;
        let committed = reservation.is_some_and(|old| old["status"] != "PENDING_PUBLISH");
        if !committed {
            match crate::core::lease::evaluate_node_lease(&target, slot.server_now()) {
                "UNLISTENED" => {
                    return Err("LISTENER_NOT_FOUND: Target listener was revoked".into())
                }
                "EXPIRED" => return Err("LISTENER_EXPIRED: Target listener expired".into()),
                _ => {}
            }
        }
        let own_principal = crate::core::auth::principal_ids_equal(
            string(&target, "principal_id"),
            string(&slot.identity, "principal_id"),
        );
        if !own_principal && target["scope"] != "workspace" {
            return Err("FORBIDDEN_TARGET_SCOPE: Target is private".into());
        }
        if committed {
            // Returning a receipt does not require a new sender enrollment or target lease.
            return Ok((target, None));
        }
        let canonical = string(&target, "listener_id");
        if let Some(caller) = caller.filter(|c| {
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
        Ok((target, from_id))
    }

    /// The UI action boundary supplies Human authority directly, never through MCP metadata.
    pub(super) fn send_from(
        &self,
        default: &str,
        args: &Value,
        caller: Option<Value>,
    ) -> Result<Value, String> {
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
        let reply = args["in_reply_to"].as_str();
        let scoped_reply = reply.and_then(|id| id.split_once(':'));
        let reply_hub = scoped_reply.map(|(name, _)| name);
        let reply_to = json!(scoped_reply.map(|(_, id)| id).or(reply));
        let request_hash = hash(&json!([message, reply_to]).to_string());
        let requested_key = args["idempotency_key"].as_str();
        let key = requested_key
            .map(str::to_owned)
            .unwrap_or_else(random_uuid_v4);
        let host = self.host_id.as_str();
        let harness = caller.as_ref().map_or("unbound", |c| string(c, "harness"));
        let session = caller
            .as_ref()
            .map_or("default", |c| string(c, "session_id"));
        let scoped_key = |principal: &str, url: &str, canonical: &str| {
            hash(&json!([principal, host, harness, session, url, canonical, key]).to_string())
        };
        let dedup_id = |slot: &crate::core::transport::HubClient, canonical: &str| {
            scoped_key(
                string(&slot.identity, "principal_id"),
                string(&slot.config, "url"),
                canonical,
            )
        };
        let check_payload = |row: &Value, hub: &str| {
            if row["request_hash"] != request_hash || reply_hub.is_some_and(|name| name != hub) {
                Err("IDEMPOTENCY_KEY_CONFLICT: Different payload".to_string())
            } else {
                Ok(())
            }
        };
        let reserved = |db: &crate::core::database::Connection,
                        hub: &str,
                        target: Option<&str>,
                        now|
         -> Result<Option<Value>, String> {
            if requested_key.is_none() {
                return Ok(None);
            }
            let slot = self.slot(hub)?;
            let old = sent_reservation(db, hub, target, now, |id| dedup_id(slot, id))?;
            if let Some(row) = &old {
                check_payload(row, hub)?;
            }
            Ok(old)
        };
        let id = crate::core::identity::canonical_node_id(to).ok();
        let guard_fallback = |db: &crate::core::database::Connection, error: &str, now| {
            if reserved(db, hub, id.as_deref(), now)?.is_some() {
                Err(error.to_owned())
            } else {
                Ok(())
            }
        };
        let implicit_default = hub != "local"
            && hub == self.config.default_hub
            && implicit_send(args, caller.as_ref());
        let local_slot = self.slots.get("local").filter(|slot| {
            implicit_default
                && slot.local.is_some()
                && !slot.closed.load(Ordering::Acquire)
                && slot.connection_status() == "connected"
        });
        // The logical route, not the eventual local target, owns a fallback retry.
        // Reuse the sender/key scope with the requested hub in place of the target.
        let fallback_key = if implicit_default && requested_key.is_some() {
            Some(dedup_id(self.slot(hub)?, hub))
        } else {
            None
        };
        let legacy_principals = std::cell::OnceCell::new();
        let local_reservation = |db: &crate::core::database::Connection, now| {
            let Some(key) = &fallback_key else {
                return Ok(None);
            };
            let mut old = db
                .query(
                    "SELECT * FROM local_sent_idempotency WHERE fallback_key=? AND expires_at_ms>?",
                    &[json!(key), json!(now)],
                )?
                .pop();
            if old.is_none() {
                db.query_while(
                    "SELECT * FROM local_sent_idempotency WHERE hub='local' AND fallback_key IS NULL AND expires_at_ms>?",
                    &[json!(now)],
                    |row| {
                        let principals = legacy_principals.get_or_init(|| -> Result<Vec<Value>, String> {
                            if let Some(slot) = local_slot {
                                return Ok(vec![slot.identity.clone()]);
                            }
                            // Disabling a slot removes its client, not its local:// send history.
                            // Retained principals can fence publication, never authorize a replay.
                            let user = self.config.effective_user("local")?;
                            db.query("SELECT DISTINCT principal_id FROM local_listeners WHERE hub='local' AND hub_url='local://' UNION SELECT ? AS principal_id", &[crate::core::identity::principal(&user)["principal_id"].clone()])
                        }).as_ref().map_err(Clone::clone)?;
                        if principals.iter().any(|identity| scoped_key(string(identity, "principal_id"), "local://", string(&row, "to_listener_id")) == row["dedup_id"]) {
                            if local_slot.is_none() {
                                return Err("HUB_UNAVAILABLE: Original local reservation cannot be verified while the local Hub is unavailable".into());
                            }
                            old = Some(row);
                        }
                        Ok(old.is_none())
                    },
                )?;
            }
            if let Some(row) = &old {
                check_payload(row, "local")?;
            }
            Ok::<_, String>(old)
        };
        let fallback_dedup = |physical: String| hash(&json!([fallback_key, physical]).to_string());
        let prepare = |hub: &str, error: Option<&str>, reservation: Option<&Value>| {
            self.prepare_send(hub, to, reply_hub, caller.as_ref(), error, reservation)
        };
        let local = if fallback_key.is_some() {
            self.state.db.read(|db| local_reservation(db, now_ms()))?
        } else {
            None
        };
        let (reservation, prepared) = if let Some(row) = local {
            (row, None)
        } else {
            let (hub, prepared, fallback_error) = match prepare(hub, None, None) {
                Ok(prepared) => (hub, prepared, None),
                Err(error) if local_slot.is_some() && error.starts_with("HUB_UNAVAILABLE:") => {
                    self.state
                        .db
                        .read(|db| guard_fallback(db, &error, now_ms()))?;
                    ("local", prepare("local", Some(&error), None)?, Some(error))
                }
                Err(error) => return Err(error),
            };
            let slot = self.slot(hub)?;
            let origin = fallback_key.as_ref().filter(|_| fallback_error.is_some());
            let physical = dedup_id(slot, string(&prepared.0, "listener_id"));
            let dedup = if origin.is_some() {
                fallback_dedup(physical)
            } else {
                physical
            };
            self.state.db.transaction(|c| {
                let now = now_ms();
                if slot.closed.load(Ordering::Acquire) {
                    return Err(STALE_HUB.into());
                }
                c.execute("DELETE FROM local_sent_idempotency WHERE expires_at_ms<=?", &[json!(now)])?;
                // Another call may have reserved on the other hub during preparation.
                if let Some(error) = &fallback_error {
                    guard_fallback(c, error, now)?;
                }
                if let Some(old) = local_reservation(c, now)? {
                    return Ok((old, None));
                }
                let canonical = string(&prepared.0, "listener_id");
                if origin.is_none() {
                    if let Some(old) = reserved(c, hub, Some(canonical), now)? {
                        return Ok((old, Some(prepared)));
                    }
                }
                let id = format!("sig_{}", random_uuid_v4().replace('-', ""));
                let expires = now + 1_800_000;
                let route_key = origin.cloned().or_else(|| (hub == "local").then(|| format!("direct:{dedup}")));
                c.execute("INSERT INTO local_sent_idempotency(hub,dedup_id,request_hash,signal_id,to_listener_id,status,expires_at_iso,expires_at_ms,fallback_key,fallback_to,fallback_public_id) VALUES(?,?,?,?,?,'PENDING_PUBLISH',?,?,?,?,?)", &[json!(hub),json!(dedup),json!(request_hash),json!(id),json!(canonical),json!(iso(expires)),json!(expires),json!(route_key),json!(origin.map(|_| to)),json!(origin.map(|_| &prepared.0["public_id"]))])?;
                let row = c.query("SELECT * FROM local_sent_idempotency WHERE hub=? AND dedup_id=?", &[json!(hub), json!(dedup)])?.pop().ok_or("Sent reservation missing")?;
                Ok((row, Some(prepared)))
            })?
        };
        let hub = string(&reservation, "hub");
        let receipt = |public_id: &str, signal_id: &str, seq: u64, warning: Option<&str>| {
            json!({
                "ok": true, "signal_id": self.scoped(hub, signal_id), "seq": seq,
                "to": self.scoped(hub, public_id), "status": "wake_accepted", "held_reason": null,
                "expires_at": reservation["expires_at_iso"], "maintenance_warning": warning
            })
        };
        let (target, from_id) = if let Some(prepared) = prepared {
            prepared
        } else if local_slot.is_some_and(|slot| {
            reservation["fallback_key"].is_null()
                || fallback_dedup(dedup_id(slot, string(&reservation, "to_listener_id")))
                    == reservation["dedup_id"]
        }) {
            prepare(hub, None, Some(&reservation))?
        } else {
            // With no original local authority, only replay a known committed receipt.
            // A different alias cannot be resolved safely without that directory.
            if reservation["fallback_to"] != to
                && id.as_deref() != reservation["to_listener_id"].as_str()
            {
                return Err("IDEMPOTENCY_KEY_CONFLICT: Different target".into());
            }
            if reservation["status"] == "PENDING_PUBLISH" {
                return Err("HUB_UNAVAILABLE: Local fallback publication is still pending".into());
            }
            return Ok(receipt(
                string(&reservation, "fallback_public_id"),
                string(&reservation, "signal_id"),
                reservation["seq"].as_u64().unwrap_or(0),
                None,
            ));
        };
        let slot = self.slot(hub)?;
        let canonical = string(&target, "listener_id");
        let dedup = string(&reservation, "dedup_id");
        let endpoint = [
            string(&slot.identity, "principal_id"),
            host,
            harness,
            session,
        ]
        .join("|");
        let mut signal_id = string(&reservation, "signal_id").to_owned();
        let mut seq = reservation["seq"].as_u64().unwrap_or(0);
        let mut maintenance_warning = None;
        if reservation["status"] == "PENDING_PUBLISH" {
            let human = caller.as_ref().is_some_and(|c| c["harness"] == "human");
            let published = if human {
                // The Hub owns reply publication and source acknowledgment in one
                // transaction. Human sends without a source use the same publication path.
                slot.rpc("send_reply_from_human", &json!({"to":canonical,"message":message,"in_reply_to":reply_to,"in_reply_to_is_raw":true,"reply_signal_id":signal_id,"msg_id":dedup,"host_id":self.host_id,"host_name":local_host_name()}))?
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
            (signal_id, seq) =
                publication_receipt(&published, (!human).then_some(&reservation["signal_id"]))?;
            // Publication is committed. Local maintenance must not invite duplicate retries.
            if self.state.db.transaction(|c| {
                if slot.closed.load(Ordering::Acquire) { return Err(STALE_HUB.into()); }
                if human && reply_to.is_string() {
                    let rows = c.query("SELECT s.*,l.hub_url AS target_hub_url,l.principal_id AS target_principal_id,l.status AS target_status FROM local_signals s JOIN local_listeners l ON s.hub=l.hub AND s.listener_id=l.listener_id WHERE s.hub=? AND s.listener_id=? AND s.signal_id=?", &[json!(hub),json!(from_id),reply_to.clone()])?;
                    if rows.first().is_some_and(super::super::state::joined_signal_is_current) {
                        c.execute("UPDATE local_signals SET acknowledged_at_ms=COALESCE(acknowledged_at_ms,?),state=CASE WHEN state='RECEIVED' THEN 'STORED_PASSIVE' ELSE state END WHERE hub=? AND listener_id=? AND signal_id=?", &[published["acked_at_ms"].clone(),json!(hub),json!(from_id),reply_to.clone()])?;
                        c.execute("DELETE FROM wake_retries WHERE hub=? AND listener_id=?", &[json!(hub),json!(from_id)])?;
                    }
                }
                c.execute("UPDATE local_sent_idempotency SET signal_id=?,seq=?,status='wake_accepted' WHERE hub=? AND dedup_id=?",&[json!(signal_id),json!(seq),json!(hub),json!(dedup)])?;
                Ok(())
            }).is_err() {
                maintenance_warning = Some("Message sent; local sent history could not be refreshed");
            }
        }
        Ok(receipt(
            string(&target, "public_id"),
            &signal_id,
            seq,
            maintenance_warning,
        ))
    }
}
impl DaemonService {
    fn inbox(&self, hub: &str, args: &Value, caller: &Value) -> Result<Value, String> {
        let limit = args["limit"].as_u64().unwrap_or(50);
        if !(1..=1000).contains(&limit) {
            return Err("INVALID_INPUT: limit must be 1..1000".into());
        }
        let mut bindings = serde_json::Map::new();
        let mut addresses = std::collections::HashMap::new();
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
                string(caller, "harness"),
                string(caller, "session_id"),
            )?;
            bindings.insert(name.clone(), json!(id));
            addresses.insert(
                name.as_str(),
                super::projection::address_peers(&self.state, slot),
            );
        }
        let bindings = Value::String(Value::Object(bindings).to_string());
        let channel = format!("{}:{}", self.host_id, string(caller, "harness"));
        self.state.db.transaction(|db| {
            self.read_and_ack_inbox(db, args, &bindings, &channel, limit as usize, &addresses)
        })
    }

    fn read_and_ack_inbox(
        &self,
        db: &crate::core::database::Connection,
        args: &Value,
        bindings: &Value,
        channel: &str,
        limit: usize,
        addresses: &std::collections::HashMap<&str, Vec<Value>>,
    ) -> Result<Value, String> {
        let ack_all = args["ack_all"]
            .as_bool()
            .unwrap_or(args["ack_ids"].is_null());
        // Sort metadata only. Load payloads by primary key until the wire page is full.
        let unread = "SELECT s.hub,s.listener_id,s.signal_id,s.hub_url,s.principal_id,l.hub_url AS target_hub_url,l.status AS target_status,l.principal_id AS target_principal_id FROM json_each(?1) b JOIN local_signals s ON s.hub=b.key AND s.listener_id=b.value JOIN local_listeners l ON l.hub=s.hub AND l.listener_id=s.listener_id WHERE s.acknowledged_at_ms IS NULL";
        let mut selected = std::collections::BTreeMap::<
            (String, String),
            std::collections::BTreeSet<String>,
        >::new();
        let mut select = |r: &Value| {
            selected
                .entry((string(r, "hub").into(), string(r, "listener_id").into()))
                .or_default()
                .insert(string(r, "signal_id").into());
        };
        // MCP embeds the JSON payload as a string. Count the escaped wire bytes;
        // reserve 1 MiB for the request ID and headroom for framing and metadata.
        struct TextBudget(usize);
        impl std::fmt::Write for TextBudget {
            fn write_str(&mut self, s: &str) -> std::fmt::Result {
                // The inner JSON serializer already escaped control characters.
                let escapes = s.bytes().filter(|b| matches!(b, b'"' | b'\\')).count();
                self.0 = self
                    .0
                    .checked_sub(s.len() + escapes)
                    .ok_or(std::fmt::Error)?;
                Ok(())
            }
        }
        use std::fmt::Write;
        let mut remaining = super::super::state::INBOX_SIGNAL_BYTES;
        let mut signals = Vec::new();
        db.query_while(&format!("{unread} ORDER BY s.created_at,s.hub,s.seq,s.signal_id"), std::slice::from_ref(bindings), |key| {
            if !super::super::state::joined_signal_is_current(&key) { return Ok(true); }
            let row = db.query("SELECT * FROM local_signals WHERE hub=? AND signal_id=?", &[key["hub"].clone(),key["signal_id"].clone()])?.pop().ok_or("STATE_ERROR: Inbox row missing")?;
            let hub = string(&row, "hub");
            let mut signal = json!({"hub":hub,"signal_id":self.scoped(hub,string(&row,"signal_id")),"seq":row["seq"],"from":{"user":row["from_user"],"host_name":row["from_host_name"],"harness":row["from_harness"],"listener_id":row["from_listener_id"],"title":row["from_title"],"about":row["from_about"]},"message":row["message"],"in_reply_to":row["in_reply_to"].as_str().map(|id|self.scoped(hub,id)),"created_at":row["created_at"],"held_reason":row["held_reason"]});
            // Reserve the two outer quotes counted by single-row admission.
            // This also covers the comma between consecutive rows in a page.
            let mut budget = TextBudget(remaining.saturating_sub(2));
            if write!(budget, "{signal}").is_err() {
                if signals.is_empty() { return Err("INVALID_INPUT: Signal exceeds inbox transport limit".into()); }
                return Ok(false);
            }
            let address = json!(super::projection::sender_display_address(
                hub, &self.config.default_hub, row["from_listener_id"].as_str(),
                string(&row, "from_user"), string(&row, "from_harness"),
                addresses.get(hub).map(Vec::as_slice).unwrap_or_default(),
            ));
            let write_address = |budget: &mut TextBudget| write!(budget, ",\"display_address\":{address}");
            let mut with_address = TextBudget(budget.0);
            signal["from"]["display_address"] = if write_address(&mut with_address).is_ok() {
                remaining = with_address.0;
                address
            } else if !signals.is_empty() {
                return Ok(false);
            } else {
                // Admission budgets the signal without the display address.
                // Keep that row readable, spending at most 1 KiB of envelope headroom.
                remaining = 0;
                if write_address(&mut TextBudget(budget.0 + 1024)).is_ok() { address } else { Value::Null }
            };
            if ack_all { select(&key); }
            signals.push(signal);
            Ok(remaining > 0 && signals.len() < limit)
        })?;
        for raw in args["ack_ids"].as_array().into_iter().flatten() {
            let raw = raw
                .as_str()
                .ok_or("INVALID_INPUT: Ack ID must be a string")?;
            let (scoped_hub, id) = match raw.split_once(':') {
                Some((h, id)) => {
                    if args["hub"].as_str().is_some_and(|hub| hub != h) {
                        return Err("INVALID_INPUT: Ack hub outside inbox".into());
                    }
                    (Some(h), id)
                }
                None => (None, raw),
            };
            let mut matched_hub = None;
            let query = format!(
                "{unread} AND (?2 IS NULL OR s.hub=?2) AND (s.signal_id=?3 OR s.batch_id=?3)"
            );
            db.query_while(
                &query,
                &[bindings.clone(), json!(scoped_hub), json!(id)],
                |row| {
                    if super::super::state::joined_signal_is_current(&row) {
                        if matched_hub.get_or_insert_with(|| row["hub"].clone()) != &row["hub"] {
                            return Err("INVALID_INPUT: Ambiguous ack ID; use hub prefix".into());
                        }
                        select(&row);
                    }
                    Ok(true)
                },
            )?;
        }
        let at = json!(now_ms());
        let mut count = 0;
        for ((hub, node), ids) in selected {
            count += ids.len();
            let keys = json!(json!(ids).to_string());
            let args = [json!(hub), json!(node), keys, at.clone(), json!(channel)];
            db.execute("UPDATE local_signals SET acknowledged_at_ms=?4,state=CASE WHEN state='RECEIVED' THEN 'STORED_PASSIVE' ELSE state END WHERE hub=?1 AND listener_id=?2 AND signal_id IN (SELECT value FROM json_each(?3))", &args[..4])?;
            db.execute("INSERT OR REPLACE INTO pending_hub_acks(hub,node_id,signal_id,channel_id,acked_at_ms,hub_url,principal_id) SELECT hub,listener_id,signal_id,?5,?4,hub_url,principal_id FROM local_signals WHERE hub=?1 AND listener_id=?2 AND signal_id IN (SELECT value FROM json_each(?3))", &args)?;
            super::super::state::reconcile_wake_retry(db, &hub, &node)?;
        }
        Ok(
            json!({"ok":true,"signals":signals,"acknowledged_count":count,"replay_gap_detected":false,"replay_gap_range":null}),
        )
    }
    pub(super) fn flush_acks(&mut self) {
        for (hub, slot) in &self.access.slots {
            let key = json!([hub, slot.config["url"], slot.identity["principal_id"]]).to_string();
            if self.ack_backoff.lock().is_ok_and(|retries| {
                retries
                    .get(&key)
                    .is_some_and(|(at, _)| *at > Instant::now())
            }) {
                continue;
            }
            let worker_key = format!("acks:{hub}");
            if self
                .receivers
                .get(&worker_key)
                .is_some_and(|w| !w.is_finished())
            {
                continue;
            }
            const PENDING: &str =
                "FROM pending_hub_acks WHERE hub=? AND hub_url=? AND principal_id IN (?,?)";
            let authority = [
                json!(hub),
                slot.config["url"].clone(),
                slot.identity["principal_id"].clone(),
                slot.identity["legacy_principal_id"].clone(),
            ];
            if !self
                .state
                .db
                .query(&format!("SELECT 1 {PENDING} LIMIT 1"), &authority)
                .is_ok_and(|rows| !rows.is_empty())
            {
                continue;
            }
            let slot = slot.clone();
            let db = self.state.db.clone();
            let backoff = self.ack_backoff.clone();
            let flush = move || {
                // Filter by authority before paging; retired profiles must not
                // starve the current one, even when their rows sort first.
                let Ok(rows) = db.query(
                    &format!("SELECT * {PENDING} ORDER BY acked_at_ms LIMIT 256"),
                    &authority,
                ) else {
                    return;
                };
                let mut failed = false;
                for row in rows {
                    match slot.rpc("signal_ack", &json!({"node_id":row["node_id"],"signal_ids":[row["signal_id"]],"channel_id":row["channel_id"]})) {
                        Ok(_) => {},
                        // This targeted 403 means the deleted registration cannot
                        // accept its ACK. Unrelated auth failures remain retryable.
                        Err(e) if e == "FORBIDDEN: target registration not found" => {},
                        Err(_) => { failed = true; break; }
                    }
                    let _ = db.execute("DELETE FROM pending_hub_acks WHERE hub=? AND node_id=? AND signal_id=? AND hub_url=? AND principal_id=? AND acked_at_ms=? AND channel_id=?", &[row["hub"].clone(),row["node_id"].clone(),row["signal_id"].clone(),row["hub_url"].clone(),row["principal_id"].clone(),row["acked_at_ms"].clone(),row["channel_id"].clone()]);
                }
                if let Ok(mut retries) = backoff.lock() {
                    if slot.closed.load(Ordering::Acquire) {
                        return;
                    }
                    if failed {
                        let retry = retries.entry(key).or_insert_with(|| {
                            (Instant::now(), crate::support::RetryBackoff::default())
                        });
                        retry.0 = Instant::now() + retry.1.failed();
                    } else {
                        retries.remove(&key);
                    }
                }
            };
            self.receivers.insert(worker_key, thread::spawn(flush));
        }
    }
}
