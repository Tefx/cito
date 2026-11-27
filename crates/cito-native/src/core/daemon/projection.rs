use super::{
    channels::remote_channels_identity,
    host_hub::{
        advertised_hub_host, host_hub_auth_mode, hub_name_from_url, lan_advertised_host,
        tailscale_advertised_host,
    },
    DaemonService, HubAccess,
};
use crate::core::{
    config::ordered_hub_names,
    hub::{channel_display_label, HubServer},
    identity::{hash, iso, string},
    state::LocalState,
    transport::HubClient,
};
use crate::plugins::now_ms;
use crate::setup::inspect_launch_at_login;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
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
    include_local_peers(state, slot, slot.cached_directory())
}
fn include_local_peers(state: &LocalState, slot: &HubClient, mut peers: Vec<Value>) -> Vec<Value> {
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

pub(super) struct HubSample {
    pub(super) directory: Vec<Value>,
    capabilities: Value,
    human_current: bool,
}
impl HubSample {
    pub(super) fn capture(slot: &HubClient, directory: Vec<Value>) -> Self {
        Self {
            directory,
            capabilities: slot.capabilities(),
            human_current: slot.human_observation_current(),
        }
    }
}

impl HubAccess {
    pub(super) fn hub_projection_config(&self) -> (Vec<String>, Map<String, Value>, Value) {
        let raw = self.raw_config().unwrap_or_default();
        let (_, mut hubs, mut rest) =
            crate::plugins::try_normalize_existing_config(Some(&raw.to_string()))
                .unwrap_or_default();
        // Keep the live projection available if the file cannot be read or normalized.
        for (name, slot) in &self.slots {
            if !hubs.contains_key(name) {
                hubs.insert(name.clone(), slot.config.clone());
            }
        }
        let order = ordered_hub_names(&hubs, raw.get("hub_order"), &self.config.default_hub);
        (order, hubs, rest.remove("host_hub").unwrap_or_default())
    }
}

impl DaemonService {
    pub fn settings(&self) -> Value {
        self.settings_for_hub(self.selected_settings_hub())
    }
    fn selected_settings_hub(&self) -> &str {
        self.active_settings_hub
            .as_deref()
            .filter(|h| self.hub_controls.contains_key(*h))
            .unwrap_or(&self.config.default_hub)
    }

    fn hub_dto(&self, name: &str, config: &Value, live: Option<(&HubClient, &HubSample)>) -> Value {
        let status = live.map_or("disconnected", |(_, sample)| {
            wire_status(string(&sample.capabilities, "status"))
        });
        let enabled = config["enabled"].as_bool().unwrap_or(true);
        let builtin = config["url"] == "local://";
        let mut hub = json!({"name":name,"url":config["url"],"auth_mode":config["authMode"],"enabled":enabled,
            "redirect_port":config["redirectPort"],"issuer":config["issuer"],"client_id":config["clientId"],
            "auto_listen":config["autoListen"].as_bool().unwrap_or(false),"status":status,"connected":enabled && status == "connected",
            "is_default":name==self.config.default_hub,"is_builtin":builtin,"supports_workspace_scope":!builtin,
            "can_login":enabled && config["authMode"] == "oidc" && status != "connected",
            "error":self.last_hub_errors.get(name).map(|(error,_)|error)});
        let projection = if let Some((slot, sample)) = live {
            hub["user"] = slot.identity["owner_user"].clone();
            hub["capabilities"] = sample.capabilities.clone();
            let record = sample.directory.iter().find(|row| {
                row["harness"] == "human"
                    && crate::core::auth::principal_matches(
                        string(row, "principal_id"),
                        &slot.identity,
                    )
            });
            hub["attachments"] = json!(record
                .map(|row| crate::core::lease::attachments(row, &self.host_id, now_ms()))
                .unwrap_or_default());
            self.project_human(
                record,
                sample.human_current,
                sample.capabilities["auth_valid"] == true && !builtin,
            )
        } else {
            hub["user"] = json!(self.config.effective_user(name).ok());
            hub["attachments"] = json!([]);
            self.project_human(None, false, false)
        };
        if let (Some(out), Value::Object(fields)) = (hub.as_object_mut(), projection) {
            out.extend(fields);
        }
        hub
    }

    fn enabled_hubs(&self, order: &[String], samples: &HashMap<&str, HubSample>) -> Vec<Value> {
        order
            .iter()
            .filter_map(|name| {
                let slot = self.slots.get(name)?;
                Some(self.hub_dto(name, &slot.config, Some((slot, &samples[name.as_str()]))))
            })
            .collect()
    }

    pub(super) fn hub_samples(&self) -> HashMap<&str, HubSample> {
        self.slots
            .iter()
            .map(|(h, s)| (h.as_str(), HubSample::capture(s, s.cached_directory())))
            .collect()
    }
    pub(super) fn settings_for_hub(&self, channel_hub: &str) -> Value {
        self.settings_snapshot(channel_hub, &self.hub_samples(), true)
    }
    fn app_settings_snapshot(&self) -> Vec<Value> {
        self.plugins.agent_plugins().into_iter().map(|p| {
            let inspection = self.inspect_setup(p);
            if let Err(error) = &inspection {
                return json!({"id":p.id,"display_name":p.display_name,"tier":p.tier,"installed":false,"paths":[],"configured":Value::Null,"status":"error","detail":error});
            }
            let (installed,configured_in_host,paths)=inspection.unwrap_or_default();
            let repair=configured_in_host && (!self.config.state_dir.join("bin/cito-mcp").is_file() || !crate::core::setup::is_staged_plugin_intact(&self.config.state_dir, p));
            let configured=configured_in_host && !repair;
            let detail=if repair { "Staged Cito files missing — click Repair to restore" }
                else if configured { p.post_setup_hint.as_deref().unwrap_or("Connected and ready") }
                else if installed { "Click Set Up to connect" } else { "Not detected; Set Up to pre-configure" };
            json!({"id":p.id,"display_name":p.display_name,"tier":p.tier,"installed":installed,"configured":configured,"paths":paths,
                "status":if repair{"repair_needed"}else if configured{"ready"}else{"not_configured"},"detail":detail})
        }).collect()
    }

    fn settings_channel_inventory(&self, hub: &str, require_current: bool) -> Value {
        if let Some(slot) = self.slots.get(hub) {
            if slot.local.is_some() {
                slot.rpc("human_channels_list", &json!({}))
                    .unwrap_or_else(|e| json!({"status":"error","detail":e}))
            } else {
                self.channel_inventories
                    .get(hub)
                    .filter(|entry| entry.identity == remote_channels_identity(slot))
                    .and_then(|entry| {
                        entry.cached.lock().ok().and_then(|cached| {
                            (!require_current || cached.current_for(slot))
                                .then(|| cached.value.clone())
                        })
                    })
                    .unwrap_or_else(|| json!({"status":"loading"}))
            }
        } else if hub == "local" {
            self.builtin_channel_inventory()
        } else {
            Value::Null
        }
    }

    fn builtin_channel_inventory(&self) -> Value {
        let user = self
            .config
            .effective_user("local")
            .unwrap_or_else(|_| "local".into());
        self.local
            .rpc(
                "human_channels_list",
                &json!({}),
                Some(&crate::core::identity::principal(&user)),
            )
            .unwrap_or_else(|e| json!({"status":"error","detail":e}))
    }

    fn channel_settings_snapshot(
        &self,
        hub: &str,
        paused: Option<bool>,
        order: &[String],
    ) -> (Vec<Value>, Option<String>) {
        let inventory = self.settings_channel_inventory(hub, false);
        let mut channels = inventory["channels"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for channel in &mut channels {
            // Suggestions are computed here, not accepted from the target Hub.
            if let Some(fields) = channel.as_object_mut() {
                fields.remove("reusable_binding");
            }
            if let Some(inbox_paused) = paused {
                super::channels::overlay_inbox_state(channel, inbox_paused);
            }
        }
        if inventory["status"] == "error" {
            // Retained rows describe the last successful read, not the current service.
            return (channels, Some(string(&inventory, "detail").to_owned()));
        }
        let mut targets = channels
            .iter_mut()
            .filter(|channel| {
                channel["hub_ready"] == true
                    && channel["configured"] != true
                    && !string(channel, "service_identity").is_empty()
                    && !string(channel, "id").is_empty()
            })
            .peekable();
        if targets.peek().is_some() {
            let sources: Vec<_> = order
                .iter()
                .filter(|name| {
                    name.as_str() != hub
                        && self
                            .slots
                            .get(*name)
                            .is_some_and(|slot| slot.capabilities()["status"] == "connected")
                })
                .map(|name| (name, self.settings_channel_inventory(name, true)))
                .collect();
            for channel in targets {
                let reusable_field = |f: &&Value| {
                    f["scope"] == "user" && f["redacted"] != true && f["type"] != "secret"
                };
                let fields = channel["fields"].as_array().into_iter().flatten();
                let keys: HashSet<_> = fields
                    .clone()
                    .filter(reusable_field)
                    .map(|f| string(f, "key"))
                    .collect();
                let suggestion = sources.iter().find_map(|(name, inventory)| {
                    let other = inventory["channels"]
                        .as_array()?
                        .iter()
                        .find(|other| {
                            other["id"] == channel["id"]
                                && other["configured"] == true
                                && other["service_identity"] == channel["service_identity"]
                        })?;
                    let values: Map<_, _> = other["fields"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(reusable_field)
                        .filter_map(|field| {
                            let key = string(field, "key");
                            let value = string(field, "value");
                            (!key.is_empty() && keys.contains(key) && !value.trim().is_empty())
                                .then(|| (key.into(), json!(value)))
                        })
                        .collect();
                    let ready = fields.clone().all(|f| f["scope"] != "user" || f["required"] != true || f["configured"] == true || values.contains_key(string(f, "key")));
                    (ready && !values.is_empty()).then(|| json!({"source_hub":name,"display_label":channel_display_label(&other["display_label"]).unwrap_or("Account"),"values":values}))
                });
                if let Some(binding) = suggestion {
                    channel["reusable_binding"] = binding;
                }
            }
        }
        (channels, None)
    }

    fn settings_snapshot(
        &self,
        hub: &str,
        samples: &HashMap<&str, HubSample>,
        include_channels: bool,
    ) -> Value {
        let apps = self.app_settings_snapshot();
        let (order, configured, host) = self.hub_projection_config();
        let hubs: Vec<_> = order
            .iter()
            .map(|name| {
                if let Some(slot) = self.slots.get(name) {
                    self.hub_dto(name, &slot.config, Some((slot, &samples[name.as_str()])))
                } else {
                    self.hub_dto(name, &configured[name], None)
                }
            })
            .collect();
        let paused = self
            .slots
            .contains_key(hub)
            .then(|| {
                hubs.iter()
                    .find(|row| row["name"] == hub)
                    .and_then(|row| row["human_paused"].as_bool())
            })
            .flatten();
        let (channels, channel_inventory_error) = if include_channels {
            self.channel_settings_snapshot(hub, paused, &order)
        } else {
            (Vec::new(), None)
        };
        let host = &host;
        let running = self.host_server.as_ref().is_some_and(HubServer::is_running);
        let host = if running {
            self.applied_host_config.as_ref().unwrap_or(host)
        } else {
            host
        };
        let port = self
            .host_server
            .as_ref()
            .filter(|_| running)
            .map(|server| u64::from(server.port()))
            .unwrap_or_else(|| host["port"].as_u64().unwrap_or(4291));
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
                if token.starts_with("--")
                    || !token
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-/=".contains(&b))
                {
                    return json!({"name":name,"url":url,"auth_mode":"token","token":token,"set_default":true}).to_string();
                }
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
        json!({"apps":apps,"settings_hub":hub,"channels":channels,"channel_inventory_error":channel_inventory_error,"hubs":hubs,"host_hub":{"enabled":host["enabled"].as_bool().unwrap_or(false),"running":running,"port":port,"endpoint_url":endpoint_url,"tailscale_url":tailscale_url,"tailscale_invite_command":tailscale_invite_command,"lan_url":lan_url,"lan_invite_command":lan_invite_command,"token":token,"invite_command":invite_command,"auth_mode":host_hub_auth_mode(host),"error":self.host_hub_error.as_ref().map(super::HostHubError::message)},"launch_at_login":inspect_launch_at_login(&self.config.home_dir)})
    }

    pub(super) fn display_address(&self, hub: &str, row: &Value) -> String {
        let peers = self
            .slots
            .get(hub)
            .map(|s| address_peers(&self.state, s))
            .unwrap_or_default();
        crate::core::identity::display_address(hub, &self.config.default_hub, row, &peers)
    }

    pub(super) fn address_book(&self) -> HashMap<&str, Vec<Value>> {
        self.slots
            .iter()
            .map(|(name, slot)| (name.as_str(), address_peers(&self.state, slot)))
            .collect()
    }

    pub(super) fn activity_dto(
        &self,
        rows: Vec<Value>,
        addresses: &HashMap<&str, Vec<Value>>,
    ) -> Vec<Value> {
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
                    json!(crate::core::identity::display_address(
                        string(&row, "hub"),
                        &self.config.default_hub,
                        &target,
                        addresses
                            .get(string(&row, "hub"))
                            .map(Vec::as_slice)
                            .unwrap_or_default()
                    ))
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
                let peers = addresses
                    .get(string(&row, "hub"))
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                row["from_display_address"] = json!(sender_display_address(
                    string(&row, "hub"),
                    &self.config.default_hub,
                    source.as_deref(),
                    string(&row, "from_user"),
                    string(&row, "from_harness"),
                    peers
                ));
                // Activity pages show previews; copy reads the canonical stored message.
                const PREVIEW_BYTES: usize = 512;
                let message = string(&row, "message");
                let preview = (message.len() > PREVIEW_BYTES).then(|| {
                    let mut end = PREVIEW_BYTES - '…'.len_utf8();
                    while !message.is_char_boundary(end) {
                        end -= 1;
                    }
                    format!("{}…", &message[..end])
                });
                row["message_truncated"] = json!(preview.is_some());
                if let Some(preview) = preview {
                    row["message"] = json!(preview);
                }
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
        self.snapshot_from(&self.hub_samples(), true)
    }

    pub(super) fn snapshot_from(
        &self,
        samples: &HashMap<&str, HubSample>,
        include_channels: bool,
    ) -> Result<Value, String> {
        let settings =
            self.settings_snapshot(self.selected_settings_hub(), samples, include_channels);
        let (activity, activity_total, activity_unread) = self
            .state
            .query_activity(&self.activity_filter, &self.activity_query)?;
        let (activity_all_total, unread) =
            if self.activity_filter == "all" && self.activity_query.trim().is_empty() {
                (activity_total, activity_unread)
            } else {
                self.state.activity_counts()?
            };
        let addresses = self.sample_addresses(samples);
        let sessions = self.local_sessions(samples, &addresses)?;
        let (available, discovery) =
            self.discovery_rows(&sessions, settings["apps"].as_array().into_iter().flatten())?;
        let activity = self.activity_dto(activity, &addresses);
        let (remote, online) = self.remote_listeners(samples, &sessions, &addresses);
        let harnesses:Vec<Value>=self.plugins.agent_plugins().into_iter().enumerate().map(|(order,p)|json!({"kind":"agent","id":p.id,"display_name":p.display_name,"sf_symbol":p.sf_symbol,"tint":p.tint,"tier":p.tier,"order":order,"pid_bound":p.pid_bound,"idle_timeout_ms":p.auto_listen.idle_timeout_ms})).collect();
        let enabled_hubs: Vec<_> = settings["hubs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|hub| self.slots.contains_key(string(hub, "name")))
            .collect();
        Ok(
            json!({"op":"state","status":if sessions.is_empty(){"idle"}else{"active"},"hubs":enabled_hubs,"hub":self.config.default_hub,"listeners":sessions,"available":available,"discovery":discovery,"my_remote_listeners":remote,"activity":activity,"activity_total":activity_total,"activity_has_any":activity_all_total>0,"activity_unread_count":activity_unread,"activity_filter":self.activity_filter,"activity_query":self.activity_query,"unread_count":unread,"directory_online_count":online,"harnesses":harnesses,"settings":settings}),
        )
    }

    fn local_sessions(
        &self,
        samples: &HashMap<&str, HubSample>,
        addresses: &HashMap<&str, Vec<Value>>,
    ) -> Result<Vec<Value>, String> {
        let mut sessions: Vec<Value> = self
            .state
            .active()?
            .into_iter()
            .filter(|row| self.slots.contains_key(string(row, "hub")))
            .collect();
        for row in &mut sessions {
            self.project_local_session(row, addresses, &samples[string(row, "hub")].capabilities)?;
            if row["harness"] == "human" {
                let directory = &samples[string(row, "hub")].directory;
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
        }
        Ok(sessions)
    }

    fn discovery_rows<'a>(
        &self,
        sessions: &[Value],
        apps: impl IntoIterator<Item = &'a Value>,
    ) -> Result<(Vec<Value>, Vec<Value>), String> {
        let unconfigured_harnesses: HashSet<_> = apps
            .into_iter()
            .filter(|app| app["configured"] == false && app["status"] != "error")
            .filter_map(|app| app["id"].as_str())
            .collect();
        let mut available = self
            .session_activity
            .as_ref()
            .map(|activity| activity.available(sessions, self.slots.len()))
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
        let discovery = self
            .session_activity
            .as_ref()
            .map(|a| {
                a.discovery_status()
                    .into_iter()
                    .filter(|r| !unconfigured_harnesses.contains(string(r, "harness")))
                    .collect()
            })
            .unwrap_or_default();
        Ok((available, discovery))
    }

    fn sample_addresses(&self, samples: &HashMap<&str, HubSample>) -> HashMap<&str, Vec<Value>> {
        self.slots
            .iter()
            .map(|(name, slot)| {
                (
                    name.as_str(),
                    include_local_peers(
                        &self.state,
                        slot,
                        samples[name.as_str()].directory.clone(),
                    ),
                )
            })
            .collect()
    }

    fn remote_listeners(
        &self,
        samples: &HashMap<&str, HubSample>,
        sessions: &[Value],
        addresses: &HashMap<&str, Vec<Value>>,
    ) -> (Vec<Value>, usize) {
        let mut remote = Vec::new();
        let mut online = 0;
        let now = now_ms();
        for (hub, slot) in &self.slots {
            let sample = &samples[hub.as_str()];
            for row in &sample.directory {
                let own = crate::core::auth::principal_ids_equal(
                    string(row, "principal_id"),
                    string(&slot.identity, "principal_id"),
                );
                if !matches!(
                    crate::core::lease::evaluate_node_lease(row, now),
                    "ACTIVE" | "DISCONNECTED_RETAINED"
                ) || (row["scope"] == "self" && !own)
                {
                    continue;
                }
                online += 1;
                if own
                    && !sessions
                        .iter()
                        .any(|s| s["hub"] == *hub && s["listener_id"] == row["listener_id"])
                {
                    let display = crate::core::identity::display_address(
                        hub,
                        &self.config.default_hub,
                        row,
                        &addresses[hub.as_str()],
                    );
                    remote.push(self.peer_dto(hub, row, &display, sample));
                }
            }
        }
        (remote, online)
    }
}
impl super::HubAccess {
    pub(super) fn peer_dto(
        &self,
        hub: &str,
        row: &Value,
        display: &str,
        sample: &HubSample,
    ) -> Value {
        let mut result = json!({"hub":hub,"listener_id":row["listener_id"],"public_id":row["public_id"],"scoped_id":self.scoped(hub,string(row,"public_id")),"display_address":display,"user":row["owner_user"],"host_id":string(row,"host_id"),"host_name":string(row,"host_name"),"harness":row["harness"],"session_id":row["session_id"],"listen_mode":row["listen_mode"].as_str().unwrap_or("manual"),"custom_title":row["custom_title"],"title":row["title"],"about":row["about"],"scope":row["scope"],"last_seen_at":row["updated_at_ms"].as_u64().into_iter().chain(row["channels"].as_object().into_iter().flat_map(|channels| channels.values()).filter_map(|channel| channel["updated_at_ms"].as_u64())).max().map(iso),"cwd":row["cwd"],"repo":row["repo"],"branch":row["branch"],"is_self_principal":self.slots.get(hub).is_some_and(|s|crate::core::auth::principal_ids_equal(string(row,"principal_id"),string(&s.identity,"principal_id")))});
        let attachments = crate::core::lease::attachments(row, &self.host_id, now_ms());
        result["channels"] = json!(attachments
            .iter()
            .filter_map(|a| a["display_name"].as_str())
            .collect::<Vec<_>>());
        result["attachments"] = json!(attachments);
        self.project_session_controls(&mut result, &sample.capabilities);
        if row["harness"] == "human" {
            let human = self.project_human(
                Some(row),
                sample.human_current,
                result["can_set_scope"] == true,
            );
            if let (Some(out), Some(fields)) = (result.as_object_mut(), human.as_object()) {
                out.extend(fields.clone());
            }
        }
        result
    }
}
impl DaemonService {
    fn project_local_session(
        &self,
        row: &mut Value,
        addresses: &HashMap<&str, Vec<Value>>,
        capabilities: &Value,
    ) -> Result<(), String> {
        *row = project_fields(row, SESSION_FIELDS);
        self.project_session_controls(row, capabilities);
        row["custom_title"] = json!(self
            .state
            .custom_title(string(row, "harness"), string(row, "session_id"))?
            .is_some());
        row["scoped_id"] = json!(self.scoped(string(row, "hub"), string(row, "public_id")));
        let hub = string(row, "hub");
        row["display_address"] = json!(crate::core::identity::display_address(
            hub,
            &self.config.default_hub,
            row,
            &addresses[hub]
        ));
        Ok(())
    }
}
impl super::HubAccess {
    fn project_session_controls(&self, row: &mut Value, capabilities: &Value) {
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
        let mutable = own && capabilities["auth_valid"] == true;
        row["is_self_principal"] = json!(own);
        row["can_set_scope"] =
            json!(mutable && slot.is_some_and(|slot| slot.config["url"] != "local://"));
        row["can_pin"] = json!(mutable && local && agent);
        row["can_rename"] = json!(mutable && local && agent);
        row["can_stop"] = json!(mutable && local && agent);
    }

    fn project_human(&self, record: Option<&Value>, current: bool, can_set_scope: bool) -> Value {
        let Some(record) = record else {
            return json!({"human_can_set_scope":false,"human_listener_id":null,"human_paused":null,"human_about":null,"human_control_revision":null,"human_availability":"UNLISTENED","human_scope":null,"human_state_status":"unknown"});
        };
        json!({"human_can_set_scope":current && can_set_scope,"human_listener_id":record["listener_id"],"human_paused":record["human_paused"].as_bool().unwrap_or(false),"human_about":record["about"],"human_control_revision":record["human_control_revision"],"human_availability":crate::core::lease::evaluate_node_lease(record,now_ms()),"human_scope":record["scope"],"human_state_status":if current{"current"}else{"stale"}})
    }
}
impl DaemonService {
    pub(super) fn emit(&self, mut frame: Value) -> bool {
        self.ui.as_ref().is_some_and(|ui| {
            super::project_companion_frame(&mut frame, &self.config.home_dir);
            ui.emit(frame)
        })
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
                    .map(|cached| {
                        let current = self
                            .slots
                            .get(name)
                            .is_some_and(|slot| cached.current_for(slot));
                        json!([name, current, hash(&cached.value.to_string())])
                    })
                    .map_err(|_| "Channel inventory lock poisoned")
            })
            .collect::<Result<_, _>>()?;
        inventories.sort_by_key(Value::to_string);
        let (order, _, _) = self.hub_projection_config();
        let mut local_inventories = Vec::new();
        if let Some(selected) = self.active_settings_hub.as_deref() {
            local_inventories.extend(order.iter().filter_map(|name| {
                let local = self
                    .slots
                    .get(name)
                    .is_some_and(|slot| slot.local.is_some());
                (local || (name == selected && selected == "local")).then(|| {
                    (
                        name.as_str(),
                        hash(&self.settings_channel_inventory(name, false).to_string()),
                    )
                })
            }));
        }
        let inspections: BTreeMap<_, _> = self
            .setup_inspections
            .iter()
            .map(|(id, entry)| (id, &entry.value))
            .collect();
        Ok(json!({"inspections": inspections, "state": [
            self.host_server.as_ref().is_some_and(HubServer::is_running),
            inventories,
            local_inventories,
            order,
            self.last_hub_errors,
            self.host_hub_error
                .as_ref()
                .map(super::HostHubError::message),
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
        ]}))
    }

    /// Keep the existing state wire shape while omitting histories and inventories.
    /// Closed surfaces retain their last full model until an explicit open refresh.
    fn status_frame(&self, mut frame: Value, inspections_changed: bool) -> Result<Value, String> {
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
        let samples = self.hub_samples();
        let addresses = self.sample_addresses(&samples);
        let sessions = self.local_sessions(&samples, &addresses)?;
        if inspections_changed {
            let (available, discovery) =
                self.discovery_rows(&sessions, &self.app_settings_snapshot())?;
            frame["available"] = json!(available);
            frame["discovery"] = json!(discovery);
        }
        let (total, unread) = self.state.activity_counts()?;
        frame["unread_count"] = json!(unread);
        frame["activity_has_any"] = json!(total > 0);
        frame["status"] = json!(if sessions.is_empty() {
            "idle"
        } else {
            "active"
        });
        let (remote, online) = self.remote_listeners(&samples, &sessions, &addresses);
        frame["my_remote_listeners"] = json!(remote);
        frame["directory_online_count"] = json!(online);
        frame["listeners"] = json!(sessions);
        let (order, _, _) = self.hub_projection_config();
        let hubs = self.enabled_hubs(&order, &samples);
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
        let inspections_changed = if let Ok(previous) = self.last_ui_revision.lock() {
            if action.is_none() && previous.as_ref() == Some(&revision) {
                return;
            }
            previous
                .as_ref()
                .is_none_or(|old| old["inspections"] != revision["inspections"])
        } else {
            true
        };
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
            self.snapshot_from(&self.hub_samples(), self.active_settings_hub.is_some())
        } else {
            self.status_frame(cached.unwrap_or_default(), inspections_changed)
        };
        if let Ok(mut frame) = result {
            let cache_update = (full || inspections_changed).then(|| frame.clone());
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
                    // The settings settlement owns correlation; this state
                    // refresh is observational and can coalesce.
                    frame.as_object_mut().unwrap().remove("request_id");
                    frame.as_object_mut().unwrap().remove("source_op");
                }
            }
            if !self.emit(frame) {
                return;
            }
            if let Some(update) = cache_update {
                if let Ok(mut cached) = self.last_ui_frame.lock() {
                    *cached = Some(update);
                }
            }
            if let Ok(mut previous) = self.last_ui_revision.lock() {
                *previous = Some(revision);
            }
        }
    }
}
