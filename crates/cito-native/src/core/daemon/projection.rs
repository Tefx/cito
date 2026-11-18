use super::{
    channels::remote_channels_identity,
    host_hub::{
        advertised_hub_host, host_hub_auth_mode, hub_name_from_url, lan_advertised_host,
        tailscale_advertised_host,
    },
    DaemonService,
};
use crate::core::{
    identity::{iso, string},
    state::LocalState,
    transport::HubClient,
};
use crate::plugins::now_ms;
use crate::setup::inspect_launch_at_login;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::Ordering;

const SESSION_FIELDS: &[&str] = &[
    "hub",
    "listener_id",
    "public_id",
    "tag",
    "principal_id",
    "owner_user",
    "host_id",
    "host_name",
    "harness",
    "session_id",
    "endpoint_pid",
    "cwd",
    "repo",
    "branch",
    "title",
    "about",
    "scope",
    "status",
    "listen_mode",
    "custom_title",
    "scoped_id",
    "display_address",
    "channels",
    "attachments",
];
const ACTIVITY_FIELDS: &[&str] = &[
    "hub",
    "signal_id",
    "listener_id",
    "from_user",
    "from_host_name",
    "from_harness",
    "from_listener_id",
    "from_title",
    "from_about",
    "message",
    "held_reason",
    "in_reply_to",
    "created_at",
    "state",
    "acknowledged_at_ms",
    "unread",
    "target_harness",
    "target_title",
    "target_scoped_id",
    "target_display_address",
    "from_scoped_id",
    "from_display_address",
    "message_truncated",
    "for_me",
    "can_reply",
];
fn project_fields(row: &Value, fields: &[&str]) -> Value {
    Value::Object(
        fields
            .iter()
            .filter_map(|key| row.get(*key).map(|value| ((*key).into(), value.clone())))
            .collect(),
    )
}

pub(super) fn wire_status(raw: &str) -> &str {
    match raw {
        "reconnecting" | "connecting" => "connecting",
        "auth_required" | "login_required" => "auth_required",
        "connected" => "connected",
        _ => "disconnected",
    }
}

// Presentation must cover local registrations before the asynchronous directory
// cache catches up. Cached Hub rows retain their lease and attachment metadata.
pub(super) fn address_peers(state: &LocalState, slot: &HubClient) -> Vec<Value> {
    let mut peers = slot.cached_directory();
    let now = now_ms();
    if let Ok(rows) = state.db.query(
        "SELECT * FROM local_listeners WHERE hub=? AND status='ACTIVE'",
        &[json!(slot.name)],
    ) {
        for mut row in rows {
            if let Some(cached) = peers
                .iter_mut()
                .find(|p| p["listener_id"] == row["listener_id"])
            {
                if !crate::core::identity::is_live_peer(cached, now) {
                    // Local registration is authoritative while the directory catches up.
                    // Preserve cached attachments, including their original lease metadata.
                    cached["state"] = json!("ACTIVE");
                    cached["status"] = json!("ACTIVE");
                    cached["updated_at_ms"] = json!(now);
                }
            } else {
                row["state"] = json!("ACTIVE");
                peers.push(row);
            }
        }
    }
    peers
}

pub(super) fn directory_identity(row: &Value) -> (String, String) {
    (string(row, "hub").into(), string(row, "listener_id").into())
}

pub(super) fn sender_display_address(
    hub: &str,
    default_hub: &str,
    source: Option<&str>,
    user: &str,
    harness: &str,
    peers: &[Value],
) -> Option<String> {
    if let Some(id) = source.filter(|id| !id.is_empty()) {
        let canonical = crate::core::identity::canonical_node_id(id).unwrap_or_default();
        if let Some(sender) = peers.iter().find(|p| p["listener_id"] == canonical) {
            return Some(crate::core::identity::display_address(
                hub,
                default_hub,
                sender,
                peers,
            ));
        }
        if !canonical.is_empty() && !user.is_empty() && !harness.is_empty() {
            let sender = json!({"owner_user":user,"harness":harness,"listener_id":canonical,"public_id":crate::core::identity::public_listener_id(user, &canonical)});
            return Some(crate::core::identity::display_address(
                hub,
                default_hub,
                &sender,
                peers,
            ));
        }
        return Some(if hub == default_hub {
            id.into()
        } else {
            format!("{hub}:{id}")
        });
    }
    (harness == "human" && !user.is_empty()).then(|| {
        crate::core::identity::display_address(
            hub,
            default_hub,
            &json!({"owner_user":user,"harness":"human"}),
            peers,
        )
    })
}

impl DaemonService {
    pub fn settings(&self) -> Value {
        let hub = self
            .active_settings_hub
            .as_deref()
            .filter(|h| self.slots.contains_key(*h))
            .unwrap_or(&self.config.default_hub);
        self.settings_for_hub(hub)
    }

    fn enabled_hub_dto(&self, name: &str, slot: &HubClient) -> Value {
        let capabilities = slot.capabilities();
        let mut hub = json!({"name":name,"url":slot.config["url"],"auth_mode":slot.config["authMode"],"enabled":true,
            "auto_listen":slot.config["autoListen"].as_bool().unwrap_or(false),"status":wire_status(string(&capabilities,"status")),
            "capabilities":capabilities,"user":slot.identity["owner_user"],"is_default":name==self.config.default_hub,
            "is_builtin":slot.config["url"] == "local://","supports_workspace_scope":slot.config["url"] != "local://",
            "can_login":slot.config["authMode"] == "oidc" && capabilities["status"] != "connected",
            "error":self.last_hub_errors.get(name).map(|(error,_)|error)});
        let record = slot.cached_directory().into_iter().find(|row| {
            row["harness"] == "human"
                && crate::core::auth::principal_matches(string(row, "principal_id"), &slot.identity)
        });
        let projection = self.project_human(
            record.as_ref(),
            capabilities["observation_current"] == true,
            capabilities["auth_valid"] == true && slot.config["url"] != "local://",
        );
        if let (Some(out), Some(fields)) = (hub.as_object_mut(), projection.as_object()) {
            out.extend(fields.clone());
        }
        hub["attachments"] = json!(record
            .as_ref()
            .map(|row| crate::core::lease::attachments(row, &self.host_id, now_ms()))
            .unwrap_or_default());
        hub
    }

    pub(super) fn settings_for_hub(&self, channel_hub: &str) -> Value {
        let apps:Vec<Value>=self.plugins.agent_plugins().into_iter().map(|p| {
            let inspection = self.inspect_setup(p);
            if let Err(error) = &inspection {
                return json!({"id":p.id,"display_name":p.display_name,"tier":p.tier,"installed":false,"paths":[],"configured":Value::Null,"status":"error","detail":error});
            }
            let (installed,configured_in_host,paths)=inspection.unwrap_or_default();
            let repair=configured_in_host && (!self.config.state_dir.join("bin/cito-mcp").is_file() || !crate::core::setup::is_staged_plugin_intact(&self.config.state_dir, p));
            let configured=configured_in_host && !repair;
            let detail=if repair { "Staged Cito files missing — click Repair to restore" }
                else if configured { p.post_setup_hint.as_deref().unwrap_or("Connected and ready") }
                else if self.last_app_action.get(&p.id).map(String::as_str)==Some("disconnect") { p.post_disconnect_hint.as_deref().unwrap_or("Disconnected · Click Set Up to reconnect") }
                else if installed { "Click Set Up to connect" } else { "Not detected; Set Up to pre-configure" };
            json!({"id":p.id,"display_name":p.display_name,"tier":p.tier,"installed":installed,"configured":configured,"paths":paths,
                "status":if repair{"repair_needed"}else if configured{"ready"}else{"not_configured"},"detail":detail})
        }).collect();
        let mut hubs = Vec::new();
        let mut channels = Vec::new();
        let mut channel_inventory_error: Option<String> = None;
        for (name, slot) in &self.slots {
            hubs.push(self.enabled_hub_dto(name, slot));
            if name == channel_hub {
                let inventory = if slot.local.is_some() {
                    slot.rpc("human_channels_list", &json!({}))
                        .unwrap_or_else(|e| json!({"status":"error","detail":e}))
                } else {
                    self.channel_inventories
                        .get(name)
                        .filter(|entry| entry.identity == remote_channels_identity(slot))
                        .and_then(|entry| {
                            entry.cached.lock().ok().map(|cached| cached.value.clone())
                        })
                        .unwrap_or_else(|| json!({"status":"loading"}))
                };
                channels = inventory["channels"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                if inventory["status"] == "error" {
                    channel_inventory_error = Some(string(&inventory, "detail").to_owned());
                }
            }
        }
        if channel_hub == "local" && !self.slots.contains_key("local") {
            let inventory = self
                .local
                .rpc(
                    "human_channels_list",
                    &json!({}),
                    Some(&crate::core::auth::local_principal(
                        &self
                            .config
                            .effective_user("local")
                            .unwrap_or_else(|_| "local".into()),
                    )),
                )
                .unwrap_or_else(|e| json!({"status":"error","detail":e}));
            channels = inventory["channels"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if inventory["status"] == "error" {
                channel_inventory_error = Some(string(&inventory, "detail").to_owned());
            }
        }
        let channel_human = hubs
            .iter()
            .find(|hub| hub["name"] == channel_hub)
            .cloned()
            .unwrap_or_else(|| self.human_projection(channel_hub));
        if let Some(inbox_paused) = channel_human["human_paused"].as_bool() {
            for channel in &mut channels {
                super::channels::overlay_inbox_state(channel, inbox_paused);
            }
        }
        let raw = self.raw_config().unwrap_or_else(|_| json!({}));
        if let Ok((_, configured, _)) =
            crate::plugins::try_normalize_existing_config(Some(&raw.to_string()))
        {
            for (name, cfg) in &configured {
                if !self.slots.contains_key(name) {
                    hubs.push(json!({"name":name,"url":cfg["url"],"auth_mode":cfg.get("authMode").or_else(||cfg.get("auth_mode")).or_else(||cfg.get("auth")).and_then(Value::as_str).unwrap_or("none").to_ascii_lowercase(),"enabled":cfg["enabled"].as_bool().unwrap_or(true),"auto_listen":cfg["autoListen"].as_bool().unwrap_or(false),"status":"disconnected","user":self.config.effective_user(name).ok(),"is_default":name==&self.config.default_hub,"error":null}));
                }
            }
        }
        let host = raw
            .get("host_hub")
            .or_else(|| raw.get("hostHub"))
            .unwrap_or(&Value::Null);
        let port = self
            .host_server
            .as_ref()
            .map(|server| u64::from(server.port()))
            .unwrap_or_else(|| host["port"].as_u64().unwrap_or(4291));
        for item in &mut hubs {
            item["error"] = json!(self
                .last_hub_errors
                .get(string(item, "name"))
                .map(|(error, _)| error));
            let cfg = self
                .slots
                .get(string(item, "name"))
                .map(|s| &s.config)
                .or_else(|| raw["hubs"].get(string(item, "name")));
            if let Some(cfg) = cfg {
                let mut cfg = cfg.clone();
                crate::core::config::canonicalize_hub_aliases(&mut cfg);
                item["redirect_port"] = cfg["redirectPort"].clone();
                item["issuer"] = cfg["issuer"].clone();
                item["client_id"] = cfg["clientId"].clone();
            }
            if item.get("human_state_status").is_none() {
                let projection = self.human_projection(string(item, "name"));
                if let (Some(item), Some(fields)) = (item.as_object_mut(), projection.as_object()) {
                    item.extend(fields.clone());
                }
            }
            item["is_builtin"] = json!(item["url"] == "local://");
            item["supports_workspace_scope"] = json!(item["url"] != "local://");
            item["can_login"] = json!(
                item["enabled"] != false
                    && item["auth_mode"] == "oidc"
                    && item["status"] != "connected"
            );
            item["connected"] = json!(item["enabled"] != false && item["status"] == "connected");
        }
        let token = string(host, "token");
        let endpoint = |host: &str| {
            let authority = if host.contains(':') {
                format!("[{host}]")
            } else {
                host.to_owned()
            };
            format!("http://{authority}:{port}")
        };
        let endpoint_url = endpoint(&advertised_hub_host(string(host, "bind")));
        let invite = |url: &str| {
            let name = hub_name_from_url(url);
            let mut command = format!("cito setup --hub {name} --url {url}");
            if host_hub_auth_mode(host) == "token" && !token.is_empty() {
                command.push_str(&format!(" --auth token --token {token}"));
            }
            command.push_str(" --default");
            command
        };
        let invite_command = invite(&endpoint_url);
        // Explicit binds only accept traffic on that interface. A Tailscale
        // alternative is valid only when the listener accepts all interfaces.
        let tailscale_url = if matches!(string(host, "bind"), "" | "0.0.0.0" | "::") {
            tailscale_advertised_host().map(|host| endpoint(&host))
        } else {
            None
        };
        let lan_url = endpoint(&lan_advertised_host(string(host, "bind")));
        let tailscale_invite_command = tailscale_url.as_deref().map(invite);
        let lan_invite_command = invite(&lan_url);
        json!({"apps":apps,"channel_hub":channel_hub,"settings_hub":channel_hub,"channels":channels,"channel_inventory_error":channel_inventory_error,"hubs":hubs,"host_hub":{"enabled":host["enabled"].as_bool().unwrap_or(false),"running":self.host_server.is_some(),"port":port,"endpoint_url":endpoint_url,"tailscale_url":tailscale_url,"tailscale_invite_command":tailscale_invite_command,"lan_url":lan_url,"lan_invite_command":lan_invite_command,"token":token,"invite_command":invite_command,"auth_mode":host_hub_auth_mode(host),"error":self.last_host_hub_error},"launch_at_login":inspect_launch_at_login(&self.config.home_dir)})
    }

    pub(super) fn display_address(&self, hub: &str, row: &Value) -> String {
        let peers = self
            .slots
            .get(hub)
            .map(|s| address_peers(&self.state, s))
            .unwrap_or_default();
        crate::core::identity::display_address(hub, &self.config.default_hub, row, &peers)
    }

    pub(super) fn activity_dto(&self, rows: Vec<Value>) -> Vec<Value> {
        rows.into_iter()
            .map(|mut row| {
                let target = self
                    .state
                    .listener(string(&row, "hub"), string(&row, "listener_id"))
                    .ok()
                    .flatten()
                    .unwrap_or(Value::Null);
                row["unread"] = json!(row["acknowledged_at_ms"].is_null());
                row["target_harness"] = json!(string(&target, "harness"));
                row["target_title"] = json!(string(&target, "title"));
                row["target_scoped_id"] =
                    json!(self.scoped(string(&row, "hub"), string(&target, "public_id")));
                row["target_display_address"] = if target.is_null() {
                    Value::Null
                } else {
                    json!(self.display_address(string(&row, "hub"), &target))
                };
                let source = row["from_listener_id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                row["from_scoped_id"] = source
                    .as_deref()
                    .map(|id| json!(self.scoped(string(&row, "hub"), id)))
                    .unwrap_or_else(|| {
                        if row["from_harness"] == "human" {
                            json!(self.scoped(
                                string(&row, "hub"),
                                &format!("@{}", string(&row, "from_user"))
                            ))
                        } else {
                            Value::Null
                        }
                    });
                let peers = self
                    .slots
                    .get(string(&row, "hub"))
                    .map(|slot| address_peers(&self.state, slot))
                    .unwrap_or_default();
                row["from_display_address"] = json!(sender_display_address(
                    string(&row, "hub"),
                    &self.config.default_hub,
                    source.as_deref(),
                    string(&row, "from_user"),
                    string(&row, "from_harness"),
                    &peers
                ));
                row["message_truncated"] = json!(string(&row, "message").len() > 512);
                row["for_me"] = json!(target["harness"] == "human");
                row["can_reply"] = json!(
                    self.slots
                        .get(string(&row, "hub"))
                        .is_some_and(|slot| slot.capabilities()["auth_valid"] == true)
                        && (row["from_scoped_id"].is_string()
                            || row["from_display_address"].is_string())
                );
                project_fields(&row, ACTIVITY_FIELDS)
            })
            .collect()
    }

    pub(super) fn live_receivers(&self) -> Result<Vec<Value>, String> {
        Ok(self
            .state
            .active()?
            .into_iter()
            .filter(|row| {
                self.slots.get(string(row, "hub")).is_some_and(|slot| {
                    !slot.closed.load(Ordering::SeqCst)
                        && slot.capabilities()["auth_valid"] == true
                        && row["hub_url"] == slot.config["url"]
                        && crate::core::auth::principal_ids_equal(
                            string(row, "principal_id"),
                            string(&slot.identity, "principal_id"),
                        )
                })
            })
            .collect())
    }

    pub fn snapshot(&self) -> Result<Value, String> {
        let settings = self.settings();
        let unconfigured_harnesses: HashSet<&str> = settings["apps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|app| app["configured"] == false && app["status"] != "error")
            .filter_map(|app| app["id"].as_str())
            .collect();
        let (activity, activity_total, activity_unread) = self
            .state
            .query_activity(&self.activity_filter, &self.activity_query)?;
        let (activity_all_total, unread) =
            if self.activity_filter == "all" && self.activity_query.trim().is_empty() {
                (activity_total, activity_unread)
            } else {
                let (_, total, unread) = self.state.query_activity("all", "")?;
                (total, unread)
            };
        let mut sessions: Vec<Value> = self
            .state
            .active()?
            .into_iter()
            .filter(|row| self.slots.contains_key(string(row, "hub")))
            .collect();
        for row in &mut sessions {
            *row = project_fields(row, SESSION_FIELDS);
            self.project_session_controls(row);
            if row["harness"] == "human" {
                let directory = self
                    .slots
                    .get(string(row, "hub"))
                    .map(|slot| slot.cached_directory())
                    .unwrap_or_default();
                if let Some(peer) = directory
                    .iter()
                    .find(|peer| peer["listener_id"] == row["listener_id"])
                {
                    let mut peer = peer.clone();
                    if let Some(channels) = peer["channels"].as_object_mut() {
                        channels.remove(&format!("{}:human", self.host_id));
                    }
                    row["channels"] =
                        json!(
                            crate::core::lease::attachments(&peer, &self.host_id, now_ms())
                                .iter()
                                .filter_map(|a| a["display_name"].as_str())
                                .collect::<Vec<_>>()
                        );
                }
            }
            row["custom_title"] = json!(self
                .state
                .custom_title(string(row, "harness"), string(row, "session_id"))?
                .is_some());
            row["scoped_id"] = json!(self.scoped(string(row, "hub"), string(row, "public_id")));
            row["display_address"] = json!(self.display_address(string(row, "hub"), row));
        }
        let mut available = self
            .session_activity
            .as_ref()
            .map(|activity| activity.available(&sessions, self.slots.len()))
            .unwrap_or_default()
            .into_iter()
            .filter(|session| !unconfigured_harnesses.contains(string(session, "harness")))
            .collect::<Vec<_>>();
        // Discovery is cached; a rename must be visible before the next tick.
        for row in &mut available {
            if let Some(title) = self
                .state
                .custom_title(string(row, "harness"), string(row, "session_id"))?
            {
                row["title"] = json!(title);
                row["custom_title"] = json!(true);
            }
        }
        let activity = self.activity_dto(activity);
        let mut remote = Vec::new();
        let mut online = 0;
        let now = now_ms();
        for (hub, slot) in &self.slots {
            for row in slot.cached_directory() {
                let own_principal = crate::core::auth::principal_ids_equal(
                    string(&row, "principal_id"),
                    string(&slot.identity, "principal_id"),
                );
                if !matches!(
                    crate::core::lease::evaluate_node_lease(&row, now),
                    "ACTIVE" | "DISCONNECTED_RETAINED"
                ) || (row["scope"] == "self" && !own_principal)
                {
                    continue;
                }
                online += 1;
                if own_principal
                    && !sessions
                        .iter()
                        .any(|s| s["hub"] == *hub && s["listener_id"] == row["listener_id"])
                {
                    let mut peer = self.peer_dto(hub, &row);
                    if row["harness"] == "human" {
                        if let Some(sample) = settings["hubs"]
                            .as_array()
                            .and_then(|hubs| hubs.iter().find(|item| item["name"] == *hub))
                        {
                            peer["human_state_status"] = sample["human_state_status"].clone();
                        }
                    }
                    remote.push(peer);
                }
            }
        }
        let harnesses:Vec<Value>=self.plugins.agent_plugins().into_iter().enumerate().map(|(order,p)|json!({"kind":"agent","id":p.id,"display_name":p.display_name,"sf_symbol":p.sf_symbol,"tint":p.tint,"tier":p.tier,"order":order,"pid_bound":p.pid_bound,"idle_timeout_ms":p.auto_listen.idle_timeout_ms})).collect();

        let enabled_hubs: Vec<Value> = settings["hubs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|hub| hub["enabled"] != false && self.slots.contains_key(string(hub, "name")))
            .cloned()
            .collect();
        Ok(
            json!({"op":"state","status":if sessions.is_empty(){"idle"}else{"active"},"hubs":enabled_hubs,"hub":self.config.default_hub,"listeners":sessions,"sessions":sessions,"available":available,"discovery":self.session_activity.as_ref().map(|a|a.discovery_status().into_iter().filter(|r|!unconfigured_harnesses.contains(string(r,"harness"))).collect::<Vec<_>>()).unwrap_or_default(),"my_remote_listeners":remote,"activity":activity,"activity_total":activity_total,"activity_all_total":activity_all_total,"activity_has_any":activity_all_total>0,"activity_unread_count":activity_unread,"activity_filter":self.activity_filter,"activity_query":self.activity_query,"unread_count":unread,"total_online":online,"directory_online_count":online,"harnesses":harnesses,"settings":settings}),
        )
    }

    pub(super) fn peer_dto(&self, hub: &str, row: &Value) -> Value {
        let mut result = json!({"hub":hub,"listener_id":row["listener_id"],"public_id":row["public_id"],"scoped_id":self.scoped(hub,string(row,"public_id")),"display_address":self.display_address(hub,row),"user":row["owner_user"],"host_id":string(row,"host_id"),"host_name":string(row,"host_name"),"harness":row["harness"],"session_id":row["session_id"],"listen_mode":row["listen_mode"].as_str().unwrap_or("manual"),"custom_title":row["custom_title"],"title":row["title"],"about":row["about"],"scope":row["scope"],"last_seen_at":row["updated_at_ms"].as_u64().into_iter().chain(row["channels"].as_object().into_iter().flat_map(|channels| channels.values()).filter_map(|channel| channel["updated_at_ms"].as_u64())).max().map(iso),"cwd":row["cwd"],"repo":row["repo"],"branch":row["branch"],"is_self_principal":self.slots.get(hub).is_some_and(|s|crate::core::auth::principal_ids_equal(string(row,"principal_id"),string(&s.identity,"principal_id")))});
        let attachments = crate::core::lease::attachments(row, &self.host_id, now_ms());
        result["channels"] = json!(attachments
            .iter()
            .filter_map(|a| a["display_name"].as_str())
            .collect::<Vec<_>>());
        result["attachments"] = json!(attachments);
        self.project_session_controls(&mut result);
        if row["harness"] == "human" {
            let current = self
                .slots
                .get(hub)
                .is_some_and(|slot| slot.observation_current());
            let human = self.project_human(Some(row), current, result["can_set_scope"] == true);
            if let (Some(out), Some(fields)) = (result.as_object_mut(), human.as_object()) {
                out.extend(fields.clone());
            }
        }
        result
    }

    fn project_session_controls(&self, row: &mut Value) {
        let slot = self.slots.get(string(row, "hub"));
        let own = slot.is_some_and(|slot| {
            crate::core::auth::principal_matches(string(row, "principal_id"), &slot.identity)
        }) || row["is_self_principal"] == true;
        let local = self
            .state
            .listener(string(row, "hub"), string(row, "listener_id"))
            .ok()
            .flatten()
            .is_some_and(|listener| listener["status"] == "ACTIVE");
        let agent = row["harness"] != "human";
        let mutable = own && slot.is_some_and(|slot| slot.capabilities()["auth_valid"] == true);
        row["is_self_principal"] = json!(own);
        row["can_set_scope"] =
            json!(mutable && slot.is_some_and(|slot| slot.config["url"] != "local://"));
        row["can_pin"] = json!(mutable && local && agent);
        row["can_rename"] = json!(mutable && local && agent);
        row["can_stop"] = json!(mutable && local && agent);
    }

    pub(super) fn human_projection(&self, hub: &str) -> Value {
        let slot = self.slots.get(hub);
        let record = slot.and_then(|slot| {
            slot.cached_directory().into_iter().find(|row| {
                row["harness"] == "human"
                    && crate::core::auth::principal_matches(
                        string(row, "principal_id"),
                        &slot.identity,
                    )
            })
        });
        let current = slot.is_some_and(|slot| slot.observation_current());
        let mut projection = self.project_human(
            record.as_ref(),
            current,
            slot.is_some_and(|slot| {
                slot.config["url"] != "local://" && slot.capabilities()["auth_valid"] == true
            }),
        );
        projection["attachments"] = json!(record
            .as_ref()
            .map(|row| crate::core::lease::attachments(row, &self.host_id, now_ms()))
            .unwrap_or_default());
        projection
    }

    fn project_human(&self, record: Option<&Value>, current: bool, can_set_scope: bool) -> Value {
        let Some(record) = record else {
            return json!({"human_can_set_scope":false,"human_listener_id":null,"human_paused":null,"human_about":null,"human_control_revision":null,"human_availability":"UNLISTENED","human_scope":null,"human_state_status":"unknown"});
        };
        json!({"human_can_set_scope":current && can_set_scope,"human_listener_id":record["listener_id"],"human_paused":record["human_paused"].as_bool().unwrap_or(false),"human_about":record["about"],"human_control_revision":record["human_control_revision"],"human_availability":crate::core::lease::evaluate_node_lease(record,now_ms()),"human_scope":record["scope"],"human_state_status":if current{"current"}else{"stale"}})
    }

    pub(super) fn emit(&self, frame: Value) {
        if let Some(ui) = &self.ui {
            ui.emit(frame);
        }
    }

    pub(super) fn push_ui(&self) {
        self.push_ui_result(None);
    }

    fn ui_revision(&self) -> Result<Value, String> {
        let changes = self
            .state
            .db
            .query("SELECT total_changes() AS changes", &[])?;
        let external = self.state.db.query("PRAGMA data_version", &[])?;
        let mut hubs: Vec<_> = self
            .slots
            .iter()
            .map(|(name, slot)| {
                json!([
                    name,
                    slot.event_revision(),
                    slot.directory_revision(),
                    slot.capabilities()
                ])
            })
            .collect();
        hubs.sort_by_key(Value::to_string);
        let mut inventories: Vec<_> = self
            .channel_inventories
            .iter()
            .map(|(name, entry)| {
                entry
                    .cached
                    .lock()
                    .map(|cached| json!([name, cached.completed_gen]))
                    .map_err(|_| "Channel inventory lock poisoned")
            })
            .collect::<Result<_, _>>()?;
        inventories.sort_by_key(Value::to_string);
        Ok(json!([
            inventories,
            self.last_hub_errors,
            self.last_host_hub_error,
            changes,
            external,
            hubs,
            self.config.default_hub,
            self.session_activity
                .as_ref()
                .map(|a| a.completion_revision()),
            self.menu_open,
            self.directory_open,
            self.active_settings_hub
        ]))
    }

    /// Keep the existing state wire shape while omitting histories and inventories.
    /// Closed surfaces retain their last full model until an explicit open refresh.
    fn status_frame(&self, cached: &Value) -> Result<Value, String> {
        let mut frame = cached.clone();
        if let Some(fields) = frame.as_object_mut() {
            for key in [
                "settings",
                "activity",
                "activity_total",
                "activity_unread_count",
                "activity_filter",
                "activity_query",
                "ok",
                "request_id",
                "source_op",
            ] {
                fields.remove(key);
            }
        }
        let mut sessions = self.state.active()?;
        sessions.retain(|row| self.slots.contains_key(string(row, "hub")));
        for row in &mut sessions {
            row["scoped_id"] = json!(self.scoped(string(row, "hub"), string(row, "public_id")));
            row["display_address"] = json!(self.display_address(string(row, "hub"), row));
            row["custom_title"] = json!(self
                .state
                .custom_title(string(row, "harness"), string(row, "session_id"))?
                .is_some());
            *row = project_fields(row, SESSION_FIELDS);
            self.project_session_controls(row);
        }
        let (_, total, unread) = self.state.query_activity("all", "")?;
        frame["unread_count"] = json!(unread);
        frame["activity_has_any"] = json!(total > 0);
        frame["activity_all_total"] = json!(total);
        frame["status"] = json!(if sessions.is_empty() {
            "idle"
        } else {
            "active"
        });
        frame["listeners"] = json!(sessions);
        frame["sessions"] = frame["listeners"].clone();
        let hubs: Vec<_> = self
            .slots
            .iter()
            .map(|(name, slot)| self.enabled_hub_dto(name, slot))
            .collect();
        frame["hubs"] = json!(hubs);
        frame["hub"] = json!(self.config.default_hub);
        Ok(frame)
    }

    pub(super) fn push_ui_result(&self, action: Option<&Value>) {
        if self.ui.is_none() {
            return;
        }
        let revision = match self.ui_revision() {
            Ok(revision) => revision,
            Err(_) => return,
        };
        if action.is_none()
            && self
                .last_ui_revision
                .lock()
                .is_ok_and(|previous| previous.as_ref() == Some(&revision))
        {
            return;
        }
        let cached = self
            .last_ui_frame
            .lock()
            .ok()
            .and_then(|frame| frame.clone());
        let full = action.is_some()
            || self.menu_open
            || self.directory_open
            || self.active_settings_hub.is_some()
            || cached.is_none();
        let result = if full {
            self.snapshot()
        } else {
            self.status_frame(cached.as_ref().unwrap_or(&Value::Null))
        };
        if let Ok(mut frame) = result {
            if full {
                if let Ok(mut cached) = self.last_ui_frame.lock() {
                    *cached = Some(frame.clone());
                }
            }
            if let Some(action) = action {
                frame["ok"] = json!(true);
                frame["request_id"] = action["request_id"].clone();
                frame["source_op"] = action["op"].clone();
                if super::http_actions::mutating_action(action) {
                    frame["committed"] = json!(true);
                }
                if super::channels::channel_action_kind(action)
                    == Some(super::channels::ChannelActionKind::Mutation)
                {
                    // The settings settlement owns correlation. This full
                    // state refresh is observational and may shed collections.
                    frame.as_object_mut().unwrap().remove("request_id");
                    frame.as_object_mut().unwrap().remove("source_op");
                }
            }
            if let Ok(mut previous) = self.last_ui_revision.lock() {
                *previous = Some(revision);
            }
            self.emit(frame);
        }
    }
}
