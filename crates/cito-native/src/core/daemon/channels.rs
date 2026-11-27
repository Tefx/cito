use super::{shutdown_requested, DaemonService};
use crate::core::{identity::string, transport::HubClient};
use crate::plugins::LoadedPlugins;
use serde_json::{json, Map, Value};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(super) struct ChannelInventory {
    pub(super) identity: Value,
    serial: Mutex<()>,
    pub(super) cached: Mutex<CachedChannelInventory>,
}

pub(super) struct CachedChannelInventory {
    pub(super) value: Value,
    pub(super) completed_gen: u64,
    refreshed_at: Instant,
    event_revision: u64,
    denied_token: Option<String>,
}

impl CachedChannelInventory {
    pub(super) fn current_for(&self, slot: &HubClient) -> bool {
        self.value["status"] != "error"
            && self.event_revision == slot.event_revision()
            && self.refreshed_at.elapsed() < Duration::from_secs(30)
    }

    pub(super) fn refresh_due(&self, slot: &HubClient, selected: bool) -> bool {
        self.authorized_for_retry(slot)
            && (self.completed_gen == 0
                || if self.value["status"] == "error" {
                    self.refreshed_at.elapsed() >= Duration::from_secs(1)
                } else {
                    !self.current_for(slot)
                        || (selected && self.refreshed_at.elapsed() >= Duration::from_secs(1))
                })
    }

    fn authorized_for_retry(&self, slot: &HubClient) -> bool {
        self.denied_token.as_ref().is_none_or(|denied| {
            *denied != crate::core::identity::hash(slot.bearer.as_deref().unwrap_or(""))
        })
    }
}

impl ChannelInventory {
    pub(super) fn new(identity: Value) -> Self {
        Self {
            identity,
            serial: Mutex::new(()),
            cached: Mutex::new(CachedChannelInventory {
                value: json!({"status":"loading"}),
                completed_gen: 0,
                refreshed_at: Instant::now(),
                event_revision: 0,
                denied_token: None,
            }),
        }
    }
    pub(super) fn refresh(&self, slot: &HubClient, observed_gen: u64) -> Result<(), String> {
        let _serial = self.serial.lock().map_err(|e| e.to_string())?;
        // A read queued behind an inventory request or mutation shares its result.
        let cached = self.cached.lock().map_err(|e| e.to_string())?;
        if !cached.authorized_for_retry(slot) {
            return Err(string(&cached.value, "detail").into());
        }
        let current = cached.completed_gen;
        drop(cached);
        if current == observed_gen {
            self.fetch_locked(slot)?;
        }
        Ok(())
    }
    fn mutate(
        &self,
        slot: &HubClient,
        plugins: &LoadedPlugins,
        action: &Value,
    ) -> Result<Value, String> {
        let _serial = self.serial.lock().map_err(|e| e.to_string())?;
        if slot.closed.load(Ordering::SeqCst) || shutdown_requested() {
            return Err("HUB_UNAVAILABLE: Channel request cancelled".into());
        }
        let mut result = run_channel_action(slot, plugins, action);
        // The same owner spans schema lookup, mutation, and the post-mutation read.
        // No pre-mutation inventory can satisfy or overwrite this acknowledgement.
        if let (Err(error), Ok(value)) = (self.fetch_locked(slot), &mut result) {
            super::mcp::add_warning(value, "maintenance_warning", &error);
        }
        result
    }
    // Both callers hold serial; cached is never held across remote I/O.
    fn fetch_locked(&self, slot: &HubClient) -> Result<(), String> {
        if slot.closed.load(Ordering::SeqCst) || shutdown_requested() {
            return Err("HUB_UNAVAILABLE: Channel request cancelled".into());
        }
        // An event during the read belongs to the next refresh.
        let event_revision = slot.event_revision();
        let result = channel_list(slot);
        if slot.closed.load(Ordering::SeqCst) || shutdown_requested() {
            return Err("HUB_UNAVAILABLE: Channel request cancelled".into());
        }
        let mut cached = self.cached.lock().map_err(|e| e.to_string())?;
        cached.completed_gen = cached
            .completed_gen
            .checked_add(1)
            .ok_or("Channel inventory generation exhausted")?;
        let refresh_error = result.as_ref().err().cloned();
        cached.denied_token = refresh_error
            .as_deref()
            .filter(|err| {
                matches!(
                    err.split_once(':').map(|(code, _)| code),
                    Some(
                        "UNAUTHORIZED"
                            | "FORBIDDEN"
                            | "AUTHORIZATION_VIOLATION"
                            | "PERMISSIONS_VIOLATION"
                            | "FORBIDDEN_TARGET_SCOPE"
                    )
                )
            })
            .map(|_| crate::core::identity::hash(slot.bearer.as_deref().unwrap_or("")));
        cached.value = match result {
            Ok(value) => value,
            Err(error) => {
                let mut value = json!({"status":"error","detail":error});
                if let Some(channels) = cached.value["channels"].as_array() {
                    value["channels"] = json!(channels);
                }
                value
            }
        };
        cached.refreshed_at = Instant::now();
        cached.event_revision = event_revision;
        refresh_error.map_or(Ok(()), Err)
    }
}

pub(super) fn overlay_inbox_state(channel: &mut Value, paused: bool) {
    channel["inbox_paused"] = json!(paused);
    channel["delivery_enabled"] =
        json!(channel["enabled"] == true && channel["configured"] == true && !paused);
    if paused
        && channel["warning"] != true
        && matches!(string(channel, "status"), "ready" | "paused")
    {
        channel["detail"] = json!("Delivery is paused while the Human Inbox is paused");
    }
}

pub(super) fn remote_channels_identity(slot: &HubClient) -> Value {
    json!([slot.config["url"], slot.identity["principal_id"]])
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ChannelActionKind {
    Read,
    Mutation,
}

pub(super) fn channel_action_kind(action: &Value) -> Option<ChannelActionKind> {
    match string(action, "op") {
        "open_settings" => Some(ChannelActionKind::Read),
        "channel_save"
        | "save_hub_service"
        | "disconnect_human_channel"
        | "channel_toggle"
        | "channel_unpair"
        | "channel_cancel_pair"
        | "channel_test"
        | "channel_pair" => Some(ChannelActionKind::Mutation),
        _ => None,
    }
}

fn channel_list(slot: &HubClient) -> Result<Value, String> {
    let inventory = slot.rpc("human_channels_list", &json!({}))?;
    if !inventory["channels"]
        .as_array()
        .is_some_and(|items| items.iter().all(Value::is_object))
    {
        return Err("INVALID_INPUT: Channel inventory must contain an array of objects".into());
    }
    Ok(inventory)
}

fn channel_id(action: &Value) -> Result<&str, String> {
    action["channel_id"]
        .as_str()
        .ok_or_else(|| "INVALID_INPUT: channel_id required".into())
}

fn channel_rpc(action: &Value) -> Result<&'static str, String> {
    let op = string(action, "op");
    let scope = action["scope"].as_str();
    if scope.is_some_and(|scope| !matches!(scope, "hub" | "user"))
        || (scope == Some("hub") && !matches!(op, "save_hub_service" | "channel_save"))
        || (scope == Some("user") && op == "save_hub_service")
    {
        return Err("INVALID_INPUT: Channel scope is incompatible with operation".into());
    }
    match op {
        "channel_pair" => Ok("human_channel_pair_start"),
        "channel_test" => Ok("human_channels_test"),
        "save_hub_service"
        | "channel_save"
        | "channel_toggle"
        | "channel_unpair"
        | "channel_cancel_pair"
        | "disconnect_human_channel" => Ok("human_channels_configure"),
        _ => Err("INVALID_INPUT: Unknown channel operation".into()),
    }
}

fn run_channel_action(
    slot: &HubClient,
    plugins: &LoadedPlugins,
    action: &Value,
) -> Result<Value, String> {
    let rpc = channel_rpc(action)?;
    let id = channel_id(action)?;
    let op = string(action, "op");
    let mut params = action.clone();
    params["plugin_id"] = json!(id);
    if let Some(values) = action["values"].as_object() {
        let inventory = channel_list(slot)?;
        let fields = inventory["channels"]
            .as_array()
            .and_then(|cs| cs.iter().find(|ch| ch["id"] == id))
            .and_then(|ch| ch["fields"].as_array())
            .or_else(|| plugins.find_by_id(id).map(|p| &p.fields))
            .ok_or_else(|| format!("INVALID_INPUT: Unknown human channel plugin: {id}"))?;
        let mut hub_config = Map::new();
        let mut user_config = Map::new();
        for field in fields {
            let key = string(field, "key");
            if let Some(value) = values.get(key) {
                if field["scope"] == "hub" {
                    if action["scope"] == "user" {
                        return Err(
                            "INVALID_INPUT: Personal values cannot change a Hub service field"
                                .into(),
                        );
                    }
                    hub_config.insert(key.into(), value.clone());
                } else {
                    user_config.insert(key.into(), value.clone());
                }
            }
        }
        if !hub_config.is_empty() {
            params["hub_config"] = Value::Object(hub_config);
        }
        if !user_config.is_empty() {
            params["user_config"] = Value::Object(user_config);
        }
    }
    if op == "channel_cancel_pair" {
        params["cancel_pair"] = json!(true);
    }
    if op == "channel_unpair" || (op == "disconnect_human_channel" && action["reset_hub"] != true) {
        params["reset_user"] = json!(true);
    }
    if op == "save_hub_service" || action["scope"] == "hub" {
        // Hub-only saves cannot create, edit, or resume a personal binding.
        let hub_config = params
            .get("hub_config")
            .cloned()
            .unwrap_or_else(|| json!({}));
        params = json!({"plugin_id":id,"hub_config":hub_config});
        if op == "channel_save" {
            if let Some(expected) = action.get("expected_service_identity") {
                params["expected_service_identity"] = expected.clone();
            }
        }
        if let Some(mode) = action.get("inbound_mode") {
            params["inbound_mode"] = mode.clone();
        }
    }
    let result = slot.rpc(rpc, &params)?;
    if !result.is_object() {
        Err("INVALID_INPUT: Channel result must be an object".into())
    } else {
        Ok(result)
    }
}

pub(super) fn dispatch_channel_action(
    service: &Arc<Mutex<DaemonService>>,
    action: &Value,
    kind: ChannelActionKind,
    ui_generation: Option<u64>,
) -> Result<Value, String> {
    let mut settled = false;
    let outcome = (|| {
        let (slot, inventory, plugins, observed_gen) = {
            let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
            s.validate_ui_generation(ui_generation)?;
            if s.closing || shutdown_requested() {
                return Err("Shutting down".into());
            }
            if action
                .get("hub")
                .is_some_and(|value| value.as_str().is_none_or(|s| s.trim().is_empty()))
            {
                return Err("INVALID_INPUT: hub must be a non-empty string".into());
            }
            if action
                .get("enabled")
                .is_some_and(|value| !value.is_boolean())
            {
                return Err("INVALID_INPUT: enabled must be a boolean".into());
            }
            if kind == ChannelActionKind::Mutation {
                channel_id(action)?;
            }
            s.reload_slots()?;
            let hub = action["hub"]
                .as_str()
                .unwrap_or(&s.config.default_hub)
                .to_string();
            s.active_settings_hub = Some(hub.clone());
            if kind == ChannelActionKind::Read {
                s.refresh_available()?;
            }
            let slot = match s.slot(&hub).cloned() {
                Ok(slot) => slot,
                Err(error) => {
                    if kind == ChannelActionKind::Read {
                        // Settings still opens for disabled hubs so they can be restored.
                        s.emit(
                            json!({"op":"settings","hub":hub,"settings":s.settings_for_hub(&hub)}),
                        );
                        return Ok(json!({"ok":true}));
                    }
                    let result = Err(error);
                    s.finish_channel_action(&hub, action, &result, true, false);
                    settled = true;
                    return result;
                }
            };
            let inventory = s.channel_inventory(&slot);
            // Pure reads can use the cached projection while this slot's owner is
            // busy. Its completion publishes fresh settings, without another RPC.
            if kind == ChannelActionKind::Read
                && matches!(
                    inventory.serial.try_lock(),
                    Err(std::sync::TryLockError::WouldBlock)
                )
            {
                s.emit(json!({"op":"settings","hub":hub,"settings":s.settings_for_hub(&hub)}));
                return Ok(json!({"ok":true}));
            }
            let observed_gen = inventory
                .cached
                .lock()
                .map_err(|e| e.to_string())?
                .completed_gen;
            (slot, inventory, s.plugins.clone(), observed_gen)
        };
        // All waits and channel I/O use the shared per-hub owner without service.
        let mut result = match kind {
            ChannelActionKind::Read => inventory
                .refresh(&slot, observed_gen)
                .map(|_| json!({"ok":true})),
            ChannelActionKind::Mutation => inventory.mutate(&slot, &plugins, action),
        };
        if kind == ChannelActionKind::Mutation {
            if let Ok(value) = &mut result {
                if value["ok"] != false {
                    value["ok"] = json!(true);
                    value["committed"] = json!(true);
                }
                value["hub"] = json!(slot.name);
                value["channel_id"] = json!(channel_id(action)?);
                value["action"] = action["op"].clone();
                value["request_id"] = action["request_id"].clone();
                value["authority_changed"] = json!(false);
            }
        }
        let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
        s.validate_ui_generation(ui_generation)?;
        if s.closing || shutdown_requested() {
            return after_channel_action(
                result,
                kind,
                "Shutting down after channel request".into(),
            );
        }
        let reload = s.reload_slots();
        // Reload may replace authority even when it fails later. Check the live
        // slot before emitting any pairing/test payload from the original owner.
        let authority_changed = slot.closed.load(Ordering::SeqCst)
            || !s
                .slots
                .get(&slot.name)
                .is_some_and(|current| remote_channels_identity(current) == inventory.identity);
        if reload.is_err() || authority_changed {
            let warning = reload
                .err()
                .unwrap_or_else(|| "Hub authority changed after channel request committed".into());
            let result = after_channel_action(result, kind, warning).map(|mut value| {
                value["authority_changed"] = json!(authority_changed);
                value
            });
            if kind == ChannelActionKind::Mutation {
                s.finish_channel_action(&slot.name, action, &result, false, authority_changed);
                settled = true;
            }
            return result;
        }
        if kind == ChannelActionKind::Mutation {
            s.finish_channel_action(&slot.name, action, &result, true, false);
            settled = true;
        } else {
            s.emit(
                json!({"op":"settings","hub":slot.name,"settings":s.settings_for_hub(&slot.name)}),
            );
        }
        result
    })();
    if !settled && outcome.is_err() && kind == ChannelActionKind::Mutation {
        let s = service.lock().map_err(|_| "Daemon lock poisoned")?;
        s.validate_ui_generation(ui_generation)?;
        let hub = action["hub"].as_str().unwrap_or(&s.config.default_hub);
        s.finish_channel_action(hub, action, &outcome, true, false);
    }
    outcome
}

fn after_channel_action(
    result: Result<Value, String>,
    kind: ChannelActionKind,
    error: String,
) -> Result<Value, String> {
    if kind == ChannelActionKind::Read {
        return Err(error);
    }
    result.map(|mut value| {
        if value["ok"] == true {
            value["committed"] = json!(true);
        }
        super::mcp::add_warning(&mut value, "maintenance_warning", &error);
        value
    })
}

impl DaemonService {
    pub(super) fn channel_inventory(&mut self, slot: &HubClient) -> Arc<ChannelInventory> {
        let identity = remote_channels_identity(slot);
        let entry = self
            .channel_inventories
            .entry(slot.name.clone())
            .or_insert_with(|| Arc::new(ChannelInventory::new(identity.clone())));
        if entry.identity != identity {
            *entry = Arc::new(ChannelInventory::new(identity));
        }
        entry.clone()
    }

    fn finish_channel_action(
        &self,
        hub: &str,
        action: &Value,
        result: &Result<Value, String>,
        project_state: bool,
        authority_changed: bool,
    ) {
        let id = channel_id(action).unwrap_or_default();
        let rpc = channel_rpc(action).unwrap_or_default();
        match result {
            _ if authority_changed => {}
            Ok(result) if matches!(rpc, "human_channel_pair_start" | "human_channels_test") => {
                let mut frame = result.clone();
                frame["op"] = json!(if rpc == "human_channel_pair_start" {
                    "pair_code"
                } else {
                    "channel_test_result"
                });
                frame["channel_id"] = json!(id);
                frame["hub"] = json!(hub);
                frame["source_op"] = action["op"].clone();
                frame["request_id"] = action["request_id"].clone();
                self.emit(frame);
            }
            Err(error) if rpc == "human_channels_test" => {
                self.emit(json!({"op":"channel_test_result","channel_id":id,"hub":hub,"action":action["op"],"source_op":action["op"],"request_id":action["request_id"],"ok":false,"error":error}));
            }
            _ => {}
        }
        let mut settlement = json!({"op":"settings","hub":hub,"channel_id":id,"action":action["op"],"ok":result.is_ok(),"source_op":action["op"],"request_id":action["request_id"],"authority_changed":authority_changed});
        if !authority_changed {
            if let Err(error) = result {
                settlement["error"] = json!(error);
            }
        }
        if let Ok(value) = result {
            settlement["committed"] = json!(value["ok"] != false);
            if let Some(warning) = value.get("maintenance_warning") {
                settlement["maintenance_warning"] = warning.clone();
            }
        }
        if let Some(warning) = settlement.get("maintenance_warning").cloned() {
            settlement["warning"] = warning;
        } else if authority_changed {
            settlement["warning"] = json!(result
                .as_ref()
                .err()
                .map(String::as_str)
                .unwrap_or("Hub authority changed after channel request"));
        }
        if project_state {
            settlement["settings"] = self.settings_for_hub(hub);
        }
        self.emit(settlement);
        if project_state {
            self.push_ui_result(Some(action));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maintenance_warnings_do_not_commit_a_failed_channel_test() {
        let failed = after_channel_action(
            Ok(json!({"ok":false,"op":"channel_test_result"})),
            ChannelActionKind::Mutation,
            "Refresh failed".into(),
        )
        .unwrap();
        assert_eq!(failed["ok"], false);
        assert!(failed.get("committed").is_none());
        let committed = after_channel_action(
            Ok(json!({"ok":true})),
            ChannelActionKind::Mutation,
            "Refresh failed".into(),
        )
        .unwrap();
        assert_eq!(committed["committed"], true);
        assert_eq!(committed["maintenance_warning"], "Refresh failed");
    }
}
