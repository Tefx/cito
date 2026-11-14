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
    pub(super) retry_at: Instant,
}

impl ChannelInventory {
    pub(super) fn new(identity: Value) -> Self {
        Self {
            identity,
            serial: Mutex::new(()),
            cached: Mutex::new(CachedChannelInventory {
                value: json!({"status":"loading"}),
                completed_gen: 0,
                retry_at: Instant::now(),
            }),
        }
    }
    pub(super) fn refresh(&self, slot: &HubClient, observed_gen: u64) -> Result<(), String> {
        let _serial = self.serial.lock().map_err(|e| e.to_string())?;
        // A read queued behind an inventory request or mutation shares its result.
        if self.cached.lock().map_err(|e| e.to_string())?.completed_gen == observed_gen {
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
            return Err("NATS_UNAVAILABLE: Channel request cancelled".into());
        }
        let result = run_channel_action(slot, plugins, action);
        // The same owner spans schema lookup, mutation, and the post-mutation read.
        // No pre-mutation inventory can satisfy or overwrite this acknowledgement.
        let refresh = self.fetch_locked(slot);
        match result {
            Ok(value) => {
                refresh?;
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }
    // Both callers hold serial; cached is never held across remote I/O.
    fn fetch_locked(&self, slot: &HubClient) -> Result<(), String> {
        if slot.closed.load(Ordering::SeqCst) || shutdown_requested() {
            return Err("NATS_UNAVAILABLE: Channel request cancelled".into());
        }
        let result = slot.rpc("human_channels_list", &json!({}));
        if slot.closed.load(Ordering::SeqCst) || shutdown_requested() {
            return Err("NATS_UNAVAILABLE: Channel request cancelled".into());
        }
        let mut cached = self.cached.lock().map_err(|e| e.to_string())?;
        cached.completed_gen = cached
            .completed_gen
            .checked_add(1)
            .ok_or("Channel inventory generation exhausted")?;
        let refresh_error = result.as_ref().err().cloned();
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
        cached.retry_at = Instant::now() + Duration::from_secs(1);
        refresh_error.map_or(Ok(()), Err)
    }
}

pub(super) fn remote_channels_identity(slot: &HubClient) -> Value {
    json!([
        slot.config["url"],
        slot.config["authMode"],
        slot.bearer,
        slot.identity["owner_user"],
        slot.identity["principal_id"]
    ])
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ChannelActionKind {
    Read,
    Mutation,
}

pub(super) fn channel_action_kind(action: &Value) -> Option<ChannelActionKind> {
    match string(action, "op") {
        "open_settings" | "settings_select_hub" => Some(ChannelActionKind::Read),
        "channel_save"
        | "save_hub_service"
        | "disconnect_human_channel"
        | "channel_toggle"
        | "channel_unpair"
        | "channel_cancel_pair"
        | "clear_channel_service"
        | "channel_test"
        | "test_human_channel"
        | "channel_pair" => Some(ChannelActionKind::Mutation),
        _ => None,
    }
}

fn channel_id(action: &Value) -> Result<&str, String> {
    action["channel_id"]
        .as_str()
        .or_else(|| action["plugin_id"].as_str())
        .ok_or_else(|| "INVALID_INPUT: channel_id required".into())
}

fn channel_rpc(action: &Value) -> &'static str {
    if action["op"] == "save_hub_service" || action["scope"] == "hub" {
        "human_channels_configure"
    } else {
        match string(action, "op") {
            "channel_pair" => "human_channel_pair_start",
            "channel_test" | "test_human_channel" => "human_channels_test",
            _ => "human_channels_configure",
        }
    }
}

fn run_channel_action(
    slot: &HubClient,
    plugins: &LoadedPlugins,
    action: &Value,
) -> Result<Value, String> {
    let id = channel_id(action)?;
    let op = string(action, "op");
    let mut params = action.clone();
    params["plugin_id"] = json!(id);
    if let Some(values) = action["values"].as_object() {
        // The selected Hub owns the schema, including remote-only plugins
        // and remote schemas that differ from a locally installed plugin.
        let schema = slot.rpc("human_channels_list", &json!({}))?;
        let fields = schema["channels"]
            .as_array()
            .and_then(|cs| cs.iter().find(|ch| ch["id"] == id))
            .and_then(|ch| ch["fields"].as_array())
            .cloned()
            .or_else(|| plugins.find_by_id(id).map(|p| p.fields.clone()))
            .ok_or_else(|| format!("INVALID_INPUT: Unknown human channel plugin: {id}"))?;
        let mut hub_config = Map::new();
        let mut user_config = Map::new();
        for field in &fields {
            let key = string(field, "key");
            if let Some(value) = values.get(key) {
                if field["scope"] == "hub" {
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
    if op == "clear_channel_service" {
        params["reset_hub"] = json!(true);
        params["reset_user"] = json!(action["reset_user"] == true);
    }
    if op == "save_hub_service" || action["scope"] == "hub" {
        // Hub-only saves cannot create, edit, or resume a personal binding.
        let hub_config = params
            .get("hub_config")
            .cloned()
            .unwrap_or_else(|| json!({}));
        params = json!({"plugin_id":id,"hub_config":hub_config});
    }
    let rpc = channel_rpc(action);
    let result = slot.rpc(rpc, &params)?;
    if matches!(rpc, "human_channel_pair_start" | "human_channels_test") && !result.is_object() {
        Err("INVALID_INPUT: Channel result must be an object".into())
    } else {
        Ok(result)
    }
}

pub(super) fn dispatch_channel_action(
    service: &Arc<Mutex<DaemonService>>,
    action: &Value,
    kind: ChannelActionKind,
) -> Result<Value, String> {
    let (slot, inventory, plugins, observed_gen) = {
        let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
        if s.closing || shutdown_requested() {
            return Err("Shutting down".into());
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
        let slot = match s.slot(&hub).cloned() {
            Ok(slot) => slot,
            Err(error) => {
                if kind == ChannelActionKind::Read {
                    // Settings still opens for disabled hubs so they can be restored.
                    s.emit(json!({"op":"settings","hub":hub,"settings":s.settings_for_hub(&hub)}));
                    return Ok(json!({"ok":true}));
                }
                let result = Err(error);
                s.finish_channel_action(&hub, action, &result);
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
    let result = match kind {
        ChannelActionKind::Read => inventory
            .refresh(&slot, observed_gen)
            .map(|_| json!({"ok":true})),
        ChannelActionKind::Mutation => inventory.mutate(&slot, &plugins, action),
    };
    let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
    if s.closing || shutdown_requested() {
        return Err("Shutting down".into());
    }
    s.reload_slots()?;
    if slot.closed.load(Ordering::SeqCst)
        || !s
            .slots
            .get(&slot.name)
            .is_some_and(|current| remote_channels_identity(current) == inventory.identity)
    {
        let result = Err("NATS_UNAVAILABLE: Hub changed during channel request".into());
        if kind == ChannelActionKind::Mutation {
            s.finish_channel_action(&slot.name, action, &result);
        }
        return result;
    }
    if kind == ChannelActionKind::Mutation {
        s.finish_channel_action(&slot.name, action, &result);
    } else {
        s.emit(json!({"op":"settings","hub":slot.name,"settings":s.settings_for_hub(&slot.name)}));
    }
    result
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

    fn finish_channel_action(&self, hub: &str, action: &Value, result: &Result<Value, String>) {
        let id = channel_id(action).unwrap_or_default();
        let rpc = channel_rpc(action);
        match result {
            Ok(result) if matches!(rpc, "human_channel_pair_start" | "human_channels_test") => {
                let mut frame = result.clone();
                frame["op"] = json!(if rpc == "human_channel_pair_start" { "pair_code" } else { "channel_test_result" });
                frame["channel_id"] = json!(id);
                frame["hub"] = json!(hub);
                frame["request_id"] = action["request_id"].clone();
                self.emit(frame);
            }
            Err(error) => self.emit(json!({"op":if rpc == "human_channels_test" {"channel_test_result"} else {"error"},"channel_id":id,"channel":id,"hub":hub,"action":action["op"],"source_op":action["op"],"request_id":action["request_id"],"ok":false,"error":error,"detail":error})),
            _ => {}
        }
        self.emit(json!({"op":"settings","hub":hub,"channel_id":id,"ok":result.is_ok(),"source_op":action["op"],"request_id":action["request_id"],"settings":self.settings_for_hub(hub)}));
        self.push_ui();
    }
}
