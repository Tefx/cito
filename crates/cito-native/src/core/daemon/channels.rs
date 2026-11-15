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
            Ok(mut value) => {
                if let Err(error) = refresh {
                    if let Some(fields) = value.as_object_mut() {
                        fields.insert("inventory_warning".into(), json!(error));
                    } else {
                        eprintln!("Channel inventory refresh: {error}");
                    }
                }
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
    json!([slot.config["url"], slot.identity["principal_id"]])
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
        "channel_test" | "test_human_channel" => Ok("human_channels_test"),
        "save_hub_service"
        | "channel_save"
        | "channel_toggle"
        | "channel_unpair"
        | "channel_cancel_pair"
        | "clear_channel_service"
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
    ui_generation: Option<u64>,
) -> Result<Value, String> {
    let (slot, inventory, plugins, observed_gen) = {
        let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
        s.validate_ui_generation(ui_generation)?;
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
        if kind == ChannelActionKind::Read {
            s.refresh_available()?;
        }
        let slot = match s.slot(&hub).cloned() {
            Ok(slot) => slot,
            Err(error) => {
                if kind == ChannelActionKind::Read {
                    // Settings still opens for disabled hubs so they can be restored.
                    s.emit(json!({"op":"settings","hub":hub,"settings":s.settings_for_hub(&hub)}));
                    return Ok(json!({"ok":true}));
                }
                let result = Err(error);
                s.finish_channel_action(&hub, action, &result, true, false);
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
            value["receipt_authority"] = json!({"hub":slot.name,"url":slot.config["url"],"principal_id":slot.identity["principal_id"]});
        }
    }
    let mut s = service.lock().map_err(|_| "Daemon lock poisoned")?;
    if s.closing || shutdown_requested() {
        return after_channel_action(result, kind, "Shutting down after channel request".into());
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
        }
        return result;
    }
    if kind == ChannelActionKind::Mutation {
        s.finish_channel_action(&slot.name, action, &result, true, false);
    } else {
        s.emit(json!({"op":"settings","hub":slot.name,"settings":s.settings_for_hub(&slot.name)}));
    }
    result
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
        if let Some(fields) = value.as_object_mut() {
            fields.insert("committed".into(), json!(true));
            fields.insert("maintenance_warning".into(), json!(error));
        } else {
            eprintln!("Channel maintenance: {error}");
        }
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
                frame["op"] = json!(if rpc == "human_channel_pair_start" { "pair_code" } else { "channel_test_result" });
                frame["channel_id"] = json!(id);
                frame["hub"] = json!(hub);
                frame["request_id"] = action["request_id"].clone();
                self.emit(frame);
            }
            Err(error) => self.emit(json!({"op":if rpc == "human_channels_test" {"channel_test_result"} else {"error"},"channel_id":id,"channel":id,"hub":hub,"action":action["op"],"source_op":action["op"],"request_id":action["request_id"],"ok":false,"error":error,"detail":error})),
            _ => {}
        }
        let mut settlement = json!({"op":"settings","hub":hub,"channel_id":id,"channel":id,"action":action["op"],"ok":result.is_ok(),"source_op":action["op"],"request_id":action["request_id"],"authority_changed":authority_changed});
        if let Ok(value) = result {
            for key in ["committed", "maintenance_warning", "receipt_authority"] {
                if let Some(value) = value.get(key) {
                    settlement[key] = value.clone();
                }
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
mod remediation_tests {
    use super::*;
    #[test]
    fn operation_routing_rejects_scope_hijacks() {
        for op in [
            "channel_pair",
            "channel_test",
            "test_human_channel",
            "channel_toggle",
            "channel_unpair",
        ] {
            assert!(channel_rpc(&json!({"op":op,"scope":"hub"}))
                .unwrap_err()
                .starts_with("INVALID_INPUT:"));
        }
        assert_eq!(
            channel_rpc(&json!({"op":"channel_pair"})).unwrap(),
            "human_channel_pair_start"
        );
        assert_eq!(
            channel_rpc(&json!({"op":"channel_test"})).unwrap(),
            "human_channels_test"
        );
        assert_eq!(
            channel_rpc(&json!({"op":"channel_save","scope":"hub"})).unwrap(),
            "human_channels_configure"
        );
    }
    #[test]
    fn dispatcher_preserves_committed_receipt_when_hub_is_disabled() -> Result<(), String> {
        dispatcher_committed_receipt("channel_toggle", false, true)
    }

    #[test]
    fn dispatcher_emits_pairing_receipt_when_post_commit_reload_fails() -> Result<(), String> {
        dispatcher_committed_receipt("channel_pair", true, false)
    }

    #[test]
    fn dispatcher_fences_pairing_receipt_when_authority_changes_and_reload_fails(
    ) -> Result<(), String> {
        dispatcher_committed_receipt("channel_pair", true, true)
    }

    #[test]
    fn dispatcher_fences_test_receipt_when_authority_changes_and_reload_fails() -> Result<(), String>
    {
        dispatcher_committed_receipt("channel_test", true, true)
    }

    #[test]
    fn dispatcher_settles_save_without_settings_when_reload_fails() -> Result<(), String> {
        dispatcher_committed_receipt("channel_save", true, false)
    }

    fn dispatcher_committed_receipt(
        op: &'static str,
        reload_failure: bool,
        replace_authority: bool,
    ) -> Result<(), String> {
        use crate::plugins::{random_uuid_v4, NativeConfig};
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;
        let dir = std::env::temp_dir().join(format!("cito-authority-{}", random_uuid_v4()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        let address = listener.local_addr().map_err(|e| e.to_string())?;
        let (committed_tx, committed_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let server = std::thread::spawn(move || loop {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let post = line.starts_with("POST");
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((key, value)) = line.split_once(':') {
                    if key.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            if !post {
                write!(
                    stream,
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                continue;
            }
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            let last = request["op"] == "human_channels_list";
            let result = if last {
                committed_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                json!({"channels":[{"id":"messenger","enabled":true}]})
            } else if request["op"] == "kv_snapshot" || request["op"] == "human_device_attach" {
                json!({"entries":[]})
            } else {
                assert_eq!(request["op"], channel_rpc(&json!({"op":op})).unwrap());
                json!({"ok":true,"committed":true,"receipt":"original","code":"original-code","instruction":"Pair with original authority"})
            };
            let body = if request["op"] == "human_device_attach" {
                json!({"ok":false,"error":{"code":"FORBIDDEN","message":"Inbox not enrolled in this channel fixture"}})
            } else { json!({"ok":true,"result":result}) }.to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            if last {
                break;
            }
        });
        let config = NativeConfig {
            state_dir: dir.join("state"),
            config_path: dir.join("config.json"),
            home_dir: dir.join("home"),
            daemon_port: 0,
            user: "alice".into(),
            default_hub: "work".into(),
        };
        let url = format!("http://{address}");
        std::fs::write(
            &config.config_path,
            json!({"default_hub":"work","hubs":{"work":{"url":url,"authMode":"none"}}}).to_string(),
        )
        .map_err(|e| e.to_string())?;
        let plugins = LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        };
        let service = Arc::new(Mutex::new(DaemonService::open(&config, &plugins)?));
        let mailbox = Arc::new(super::super::UiMailbox::default());
        service.lock().unwrap().ui = Some(mailbox.clone());
        let dispatcher = {
            let service = service.clone();
            std::thread::spawn(move || {
                dispatch_channel_action(
                    &service,
                    &json!({"op":op,"hub":"work","channel_id":"messenger","enabled":true,"request_id":"original-request"}),
                    ChannelActionKind::Mutation,
                    None,
                )
            })
        };
        committed_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        {
            let mut s = service.lock().unwrap();
            if reload_failure {
                // Gate the committed receipt until a concurrent config change
                // and an incomplete external edit have reached the daemon.
                if replace_authority {
                    s.mutate_config(|raw| {
                        raw["hubs"]["work"]["url"] = json!("local://");
                        Ok(())
                    })?;
                    s.reload_slots()?;
                }
                std::fs::write(&config.config_path, "{").map_err(|e| e.to_string())?;
            } else {
                s.mutate_config(|raw| {
                    raw["hubs"]["work"]["enabled"] = json!(false);
                    Ok(())
                })?;
                s.reload_slots()?;
                assert!(!s.slots.contains_key("work"));
            }
        }
        release_tx.send(()).map_err(|e| e.to_string())?;
        let result = dispatcher.join().map_err(|_| "Dispatcher panicked")??;
        server.join().map_err(|_| "Authority server panicked")?;
        assert_eq!(result["receipt"], "original");
        assert_eq!(result["committed"], true);
        assert_eq!(result["authority_changed"], replace_authority);
        assert_eq!(
            result["receipt_authority"],
            json!({"hub":"work","url":url,"principal_id":"none#alice"})
        );
        assert!(result["maintenance_warning"].is_string());
        {
            let mut s = service.lock().unwrap();
            if replace_authority && reload_failure {
                assert_eq!(s.slots["work"].config["url"], "local://");
            }
            if !replace_authority && op == "channel_pair" {
                let frame: Value =
                    serde_json::from_slice(&mailbox.next().unwrap()).map_err(|e| e.to_string())?;
                assert_eq!(frame["op"], "pair_code");
                assert_eq!(frame["code"], "original-code");
                assert_eq!(frame["request_id"], "original-request");
            }
            assert_eq!(mailbox.pending.lock().unwrap().len(), 1);
            let frame: Value =
                serde_json::from_slice(&mailbox.next().unwrap()).map_err(|e| e.to_string())?;
            assert_eq!(frame["op"], "settings");
            assert_eq!(frame["request_id"], "original-request");
            assert_eq!(frame["hub"], "work");
            assert_eq!(frame["channel"], "messenger");
            assert_eq!(frame["action"], op);
            assert_eq!(frame["committed"], true);
            assert_eq!(frame["authority_changed"], replace_authority);
            assert_eq!(frame["receipt_authority"], result["receipt_authority"]);
            assert!(frame["warning"].is_string());
            for field in ["settings", "code", "instruction", "detail"] {
                assert!(frame.get(field).is_none());
            }
            assert!(mailbox.pending.lock().unwrap().is_empty());
            s.shutdown();
        }
        std::fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
        Ok(())
    }

    #[test]
    fn committed_channel_result_survives_inventory_failure() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            for expected in ["human_channels_configure", "human_channels_list"] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(':') {
                        if key.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap();
                        }
                    }
                }
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).unwrap();
                let request: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(request["op"], expected);
                let body = if expected == "human_channels_configure" { json!({"ok":true,"result":{"ok":true,"committed":true}}) } else { json!({"ok":false,"error":{"code":"UNAVAILABLE","message":"Inventory unavailable"}}) }.to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let slot = HubClient::new(
            "work".into(),
            json!({"url":format!("http://{address}"),"authMode":"none"}),
            "alice",
            None,
        );
        let inventory = ChannelInventory::new(remote_channels_identity(&slot));
        let plugins = LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        };
        let result = inventory
            .mutate(
                &slot,
                &plugins,
                &json!({"op":"channel_toggle","channel_id":"sample","enabled":true}),
            )
            .unwrap();
        worker.join().unwrap();
        assert_eq!(result["committed"], true);
        assert!(result["inventory_warning"]
            .as_str()
            .unwrap()
            .contains("Inventory unavailable"));
        assert_eq!(inventory.cached.lock().unwrap().value["status"], "error");
    }
}
