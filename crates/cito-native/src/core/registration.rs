//! Shared registration serializes background enrollment with explicit local intent.
use super::{
    auth::principal_ids_equal,
    identity::{listener_id, listener_id_candidates, local_host_name, public_listener_id, string},
    state::LocalState,
    transport::HubClient,
};
use crate::plugins::{now_ms, random_uuid_v4};
use serde_json::{json, Value};

pub fn validate_title(title: &str) -> Result<(), String> {
    if title.trim().is_empty() || title.chars().count() > 128 {
        return Err("INVALID_INPUT: title must contain 1..128 characters".into());
    }
    Ok(())
}

/// Reconcile durable naming intent without replacing another host's binding.
pub fn sync_custom_title(
    state: &LocalState,
    client: &HubClient,
    host_id: &str,
    row: &Value,
) -> Result<(), String> {
    let Some(title) = state.custom_title(string(row, "harness"), string(row, "session_id"))? else {
        return Ok(());
    };
    let _registration = client
        .registration
        .lock()
        .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
    state.db.execute("UPDATE local_listeners SET title=? WHERE hub=? AND listener_id=? AND binding_generation=? AND owner_token=?", &[json!(title),row["hub"].clone(),row["listener_id"].clone(),row["binding_generation"].clone(),row["owner_token"].clone()])?;
    let channel = format!("{host_id}:{}", string(row, "harness"));
    for _ in 0..3 {
        let Some((mut remote, revision)) = client.kv_get(string(row, "listener_id"))? else {
            return Ok(());
        };
        if remote["title"] == title && remote["custom_title"] == true {
            return Ok(());
        }
        if !remote_matches_listener(&remote, row, &channel) {
            return Ok(());
        }
        remote["title"] = json!(title);
        remote["custom_title"] = json!(true);
        match client.kv_save(string(row, "listener_id"), &remote, Some(revision)) {
            Ok(_) => return Ok(()),
            Err(e) if e.starts_with("10071:") => continue,
            Err(e) => return Err(e),
        }
    }
    Err("STALE_GENERATION: Title CAS retries exhausted".into())
}

pub fn local_node(
    state: &LocalState,
    client: &HubClient,
    harness: &str,
    session: &str,
) -> Result<Option<Value>, String> {
    Ok(state.db.query(
        "SELECT * FROM local_listeners WHERE hub=? AND harness=? AND session_id=? ORDER BY status='ACTIVE' DESC,binding_generation DESC",
        &[json!(client.name), json!(harness), json!(session)],
    )?.into_iter().find(|row| principal_ids_equal(string(row,"principal_id"), string(&client.identity,"principal_id"))))
}

pub fn local_node_id(
    state: &LocalState,
    client: &HubClient,
    harness: &str,
    session: &str,
) -> Result<String, String> {
    Ok(local_node(state, client, harness, session)?
        .map(|row| string(&row, "listener_id").to_string())
        .unwrap_or_else(|| listener_id(string(&client.identity, "principal_id"), harness, session)))
}

pub(super) fn remote_matches_listener(remote: &Value, row: &Value, channel: &str) -> bool {
    remote["binding_generation"] == row["binding_generation"]
        && remote["channels"][channel]["owner_token"]
            .as_str()
            // Registrations written before channel-scoped ownership use the Node token.
            .or_else(|| remote["owner_token"].as_str())
            .is_some_and(|owner| row["owner_token"] == owner)
}

pub fn register_listener(
    state: &LocalState,
    client: &HubClient,
    host_id: &str,
    args: &Value,
    caller: &Value,
) -> Result<Value, String> {
    let _registration = client
        .registration
        .lock()
        .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
    register_locked(state, client, host_id, args, caller, true)
}

pub(super) fn distinct_human_about<'a>(about: &'a str, title: &str, user: &str) -> Option<&'a str> {
    let about = about.trim();
    let lower = about.to_lowercase();
    let user = user.trim().to_lowercase();
    // Description aliases use the same process HOME basename as the UI.
    let home = std::env::var("HOME").unwrap_or_default();
    let home = home
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or("")
        .to_lowercase();
    (!about.is_empty()
        && ![
            "human",
            "human node",
            "human channel",
            "human receiver",
            "you",
            "session",
            "coding session",
            "default",
            "agent",
            "agent session",
            "cli session",
        ]
        .contains(&lower.as_str())
        && lower != title.trim().to_lowercase()
        && lower != user
        && lower != format!("@{user}")
        && lower != format!("you (@{user})")
        && lower != home
        && lower != format!("@{home}"))
    .then_some(about)
}

pub fn register_automatic_human(
    state: &LocalState,
    client: &HubClient,
    host_id: &str,
    args: &Value,
    caller: &Value,
) -> Result<Value, String> {
    let _registration = client
        .registration
        .lock()
        .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
    let mut args = args.clone();
    let user = string(&client.identity, "owner_user");
    if distinct_human_about(string(&args, "about"), string(caller, "title"), user).is_none() {
        args["about"] = json!("Human receiver");
    }
    if let Some(row) =
        local_node(state, client, "human", "default")?.filter(|row| row["status"] == "UNLISTENED")
    {
        // A pending local pause owns intent until its detach settles. Afterwards,
        // an active sibling with the global pause cleared represents a remote resume.
        let pending = !state
            .db
            .query(
                "SELECT 1 FROM pending_hub_detaches WHERE hub=? AND listener_id=?",
                &[json!(client.name), row["listener_id"].clone()],
            )?
            .is_empty();
        let channel = format!("{host_id}:human");
        let remote = if pending {
            None
        } else {
            client.kv_get(string(&row, "listener_id"))?
        };
        let resumed = remote.as_ref().is_some_and(|(remote, _)| {
            remote["human_paused"] != true
                && remote["state"] == "ACTIVE"
                && principal_ids_equal(
                    string(&remote, "principal_id"),
                    string(&client.identity, "principal_id"),
                )
                && remote["channels"].as_object().is_some_and(|channels| {
                    channels
                        .iter()
                        .any(|(id, ch)| id != &channel && ch["state"] == "ACTIVE")
                })
        });
        if !resumed {
            return Err("BINDING_REQUIRED: Human enrollment was explicitly disabled".into());
        }
        if distinct_human_about(string(&args, "about"), string(caller, "title"), user).is_none() {
            // Resolve remote fallback inside the CAS loop so a retry uses current metadata.
            args["about"] =
                json!(
                    distinct_human_about(string(&row, "about"), string(&row, "title"), user)
                        .unwrap_or("Human receiver")
                );
        }
    }
    register_locked(state, client, host_id, &args, caller, false)
}

fn register_locked(
    state: &LocalState,
    client: &HubClient,
    host_id: &str,
    args: &Value,
    caller: &Value,
    resume_human: bool,
) -> Result<Value, String> {
    let hub = &client.name;
    let about = string(args, "about");
    if about.trim().is_empty() || about.chars().count() > 500 {
        return Err("INVALID_INPUT: about must contain 1..500 characters".into());
    }
    if let Some(tag) = args.get("tag") {
        if !tag
            .as_str()
            .is_some_and(|t| !t.trim().is_empty() && t.chars().count() <= 64)
        {
            return Err("INVALID_INPUT: tag must contain 1..64 characters".into());
        }
    }
    let harness = string(caller, "harness");
    let session = string(caller, "session_id");
    if harness.is_empty() || session.is_empty() || (harness == "human" && session != "default") {
        return Err("INVALID_INPUT: Invalid Node session identity".into());
    }
    if let Some(title) = args.get("title") {
        let title = title
            .as_str()
            .ok_or("INVALID_INPUT: title must be a string")?;
        state.set_custom_title(harness, session, title)?;
    }
    let custom_title = state.custom_title(harness, session)?;
    let title = custom_title
        .as_deref()
        .unwrap_or_else(|| match string(caller, "title") {
            title if harness == "human" && title.trim().is_empty() => "Human",
            title => title,
        });
    let initial_local = local_node(state, client, harness, session)?;
    let mut id = initial_local
        .as_ref()
        .map(|row| string(row, "listener_id").to_string())
        .unwrap_or_else(|| listener_id(string(&client.identity, "principal_id"), harness, session));
    let mut initial_remote = client.kv_get(&id)?;
    if initial_local.is_none() && initial_remote.is_none() {
        for legacy in listener_id_candidates(&client.identity, harness, session) {
            if legacy == id {
                continue;
            }
            if let Some(remote) = client.kv_get(&legacy)? {
                id = legacy;
                initial_remote = Some(remote);
                break;
            }
        }
    }
    let tag = args
        .get("tag")
        .cloned()
        .or_else(|| initial_local.as_ref().map(|row| row["tag"].clone()))
        .unwrap_or(Value::Null);
    let public = public_listener_id(string(&client.identity, "owner_user"), &id);
    let channel = format!("{host_id}:{harness}");
    let host_name = local_host_name();
    for attempt in 0..3 {
        if client.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("NATS_UNAVAILABLE: Hub disconnected during registration".into());
        }
        let remote = if attempt == 0 {
            initial_remote.take()
        } else {
            client.kv_get(&id)?
        };
        let previous = remote
            .as_ref()
            .map(|r| r.0.clone())
            .unwrap_or_else(|| json!({}));
        if !previous.is_object() {
            return Err("INVALID_INPUT: Listener must be an object".into());
        }
        if remote.is_some()
            && (!principal_ids_equal(
                string(&previous, "principal_id"),
                string(&client.identity, "principal_id"),
            ) || string(&previous, "harness") != harness
                || string(&previous, "session_id") != session
                || (!string(&previous, "listener_id").is_empty() && previous["listener_id"] != id))
        {
            return Err("STALE_GENERATION: Listener collision".into());
        }
        let about = if harness == "human"
            && !resume_human
            && distinct_human_about(about, title, string(&client.identity, "owner_user")).is_none()
        {
            distinct_human_about(
                string(&previous, "about"),
                string(&previous, "title"),
                string(&client.identity, "owner_user"),
            )
            .unwrap_or("Human receiver")
        } else {
            about
        };
        let local = local_node(state, client, harness, session)?;
        let scope = args["scope"]
            .as_str()
            .or_else(|| local.as_ref().and_then(|row| row["scope"].as_str()))
            .or_else(|| {
                previous["scope"]
                    .as_str()
                    .filter(|scope| matches!(*scope, "self" | "workspace"))
            })
            .unwrap_or(
                if client.config["authMode"]
                    .as_str()
                    .or_else(|| client.config["auth_mode"].as_str())
                    .unwrap_or("none")
                    == "none"
                {
                    "workspace"
                } else {
                    "self"
                },
            );
        if !matches!(scope, "self" | "workspace") {
            return Err("INVALID_INPUT: Invalid scope".into());
        }
        if harness == "human" && !resume_human && previous["human_paused"] == true {
            return Err("BINDING_REQUIRED: Personal notifications are paused on this Hub".into());
        }
        let sibling = previous["channels"].as_object().is_some_and(|channels| {
            channels
                .iter()
                .any(|(key, value)| key != &channel && value["state"] == "ACTIVE")
        });
        let same_binding = local.as_ref().filter(|row| {
            row["status"] == "ACTIVE"
                // An explicit resume must invalidate a heartbeat that already saw
                // the remote pause while this Mac's local row was still active.
                && (harness != "human" || previous["human_paused"] != true)
                && previous["state"] == "ACTIVE"
                && previous["channels"][&channel]["state"] == "ACTIVE"
                && previous["channels"][&channel]["owner_token"] == row["owner_token"]
                && remote_matches_listener(&previous, row, &channel)
                && row["endpoint_pid"] == caller["endpoint_pid"]
        });
        let retain = (sibling || same_binding.is_some())
            && previous["binding_generation"]
                .as_u64()
                .is_some_and(|v| v > 0);
        let generation = if retain {
            previous["binding_generation"].as_u64().unwrap_or(0)
        } else {
            previous["binding_generation"]
                .as_u64()
                .unwrap_or(0)
                .max(
                    local
                        .as_ref()
                        .and_then(|row| row["binding_generation"].as_u64())
                        .unwrap_or(0),
                )
                .checked_add(1)
                .filter(|v| *v <= i64::MAX as u64)
                .ok_or("STALE_GENERATION: Binding generation exhausted")?
        };
        let owner = same_binding
            .map(|row| string(row, "owner_token").to_string())
            .unwrap_or_else(random_uuid_v4);
        // Keep legacy sibling owners valid while replacing only this channel's token.
        let node_owner = if sibling {
            previous["owner_token"].clone()
        } else {
            json!(owner)
        };
        let mut channels = previous["channels"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        channels.insert(channel.clone(),json!({"channel_id":channel,"kind":harness,"host_id":host_id,"host_name":host_name,"delivery_mode":if harness=="human"{"notify_fanout"}else{"wake_exclusive"},"priority":100,"state":"ACTIVE","owner_token":owner,"updated_at_ms":now_ms()}));
        let mut value = json!({"listener_id":id,"public_id":public,"tag":tag,"principal_id":client.identity["principal_id"],"owner_user":client.identity["owner_user"],"host_id":host_id,"host_name":host_name,"harness":harness,"session_id":session,"cwd":string(caller,"cwd"),"repo":caller["repo"],"branch":caller["branch"],"title":title,"about":about,"scope":scope,"state":"ACTIVE","binding_generation":generation,"owner_token":node_owner,"channels":channels});
        value["custom_title"] = json!(custom_title.is_some());
        value["human_paused"] = if harness == "human" && resume_human {
            json!(false)
        } else {
            previous["human_paused"].clone()
        };
        super::lease::reconcile_registration_state(&mut value, now_ms());
        match client.kv_save(&id, &value, remote.map(|r| r.1)) {
            Ok(_) => {}
            Err(error) if error.starts_with("10071:") => continue,
            Err(error) => return Err(error),
        }
        let committed = state.db.transaction(|connection| {
            // Close precedes profile fencing under this same database mutex.
            if client.closed.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("NATS_UNAVAILABLE: Hub disconnected during registration".into());
            }
            // The remote CAS cannot authorize reuse after a concurrent local revocation.
            if same_binding.is_some() && connection.query(
                "SELECT 1 FROM local_listeners WHERE hub=? AND listener_id=? AND status='ACTIVE' AND binding_generation=? AND COALESCE(owner_token,'')=?",
                &[json!(hub), json!(id), json!(generation), json!(owner)],
            )?.is_empty() { return Ok(false); }
            let changed = connection.execute(
                "INSERT INTO local_listeners(hub,listener_id,public_id,tag,principal_id,owner_user,host_id,host_name,harness,session_id,endpoint_pid,cwd,repo,branch,title,about,scope,status,binding_generation,owner_token) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'ACTIVE',?,?) ON CONFLICT(hub,listener_id) DO UPDATE SET consecutive_probe_failures=CASE WHEN status!='ACTIVE' OR owner_token!=excluded.owner_token OR COALESCE(endpoint_pid,-1)!=COALESCE(excluded.endpoint_pid,-1) THEN 0 ELSE consecutive_probe_failures END,public_id=excluded.public_id,tag=excluded.tag,owner_user=excluded.owner_user,host_id=excluded.host_id,host_name=excluded.host_name,endpoint_pid=excluded.endpoint_pid,cwd=excluded.cwd,repo=excluded.repo,branch=excluded.branch,title=excluded.title,about=excluded.about,scope=excluded.scope,status='ACTIVE',binding_generation=excluded.binding_generation,owner_token=excluded.owner_token WHERE local_listeners.status!='ACTIVE' OR local_listeners.binding_generation<=excluded.binding_generation",
                &[json!(hub),json!(id),json!(public),tag.clone(),client.identity["principal_id"].clone(),client.identity["owner_user"].clone(),json!(host_id),json!(host_name),json!(harness),json!(session),caller["endpoint_pid"].clone(),json!(string(caller,"cwd")),caller["repo"].clone(),caller["branch"].clone(),json!(title),json!(about),json!(scope),json!(generation),json!(owner)],
            )?;
            if changed == 0 { return Err("STALE_GENERATION: Local binding was replaced".into()); }
            connection.execute("UPDATE local_listeners SET hub_url=? WHERE hub=? AND listener_id=?", &[client.config["url"].clone(),json!(hub),json!(id)])?;
            Ok(true)
        })?;
        if !committed {
            continue;
        }
        return Ok(
            json!({"ok":true,"status":"listening","hub":hub,"listener_id":public,"canonical_listener_id":id,"binding_generation":generation,"user":client.identity["owner_user"],"host_id":host_id,"host_name":host_name,"harness":harness,"session_id":session,"about":about,"repo":caller["repo"],"branch":caller["branch"],"title":title,"scope":scope,"lease_ttl_seconds":60}),
        );
    }
    Err("STALE_GENERATION: Listener CAS retries exhausted".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::hub::HubEngine;
    use std::{fs, sync::Arc};

    #[test]
    fn human_title_defaults_reach_response_local_state_and_hub_without_changing_custom_titles(
    ) -> Result<(), String> {
        let root = std::env::temp_dir().join(format!("cito-registration-{}", random_uuid_v4()));
        let engine = Arc::new(HubEngine::open(&root.join("hub"))?);
        engine.set_plugin_roots(&[])?;
        let result = (|| {
            let state = LocalState::open(&root.join("state"))?;
            let client = HubClient::new(
                "work".into(),
                json!({"url":"local://"}),
                "alice",
                Some(engine.clone()),
            );
            for (harness, title, expected) in [
                ("human", Value::Null, "Human"),
                ("human", json!(""), "Human"),
                ("human", json!(" \n\t"), "Human"),
                ("human", json!("Review desk"), "Review desk"),
                ("fixture", json!(""), ""),
                ("fixture", json!("Agent title"), "Agent title"),
            ] {
                let response = register_listener(
                    &state,
                    &client,
                    "host",
                    &json!({"about":"Human receiver"}),
                    &json!({"harness":harness,"session_id":"default","title":title}),
                )?;
                let id = string(&response, "canonical_listener_id");
                assert_eq!(response["title"], expected);
                assert_eq!(
                    state.listener("work", id)?.ok_or("Missing local row")?["title"],
                    expected
                );
                assert_eq!(
                    client.kv_get(id)?.ok_or("Missing hub row")?.0["title"],
                    expected
                );
            }
            let caller =
                json!({"harness":"fixture","session_id":"default","title":"Discovered title"});
            register_listener(
                &state,
                &client,
                "host",
                &json!({"about":"Work","title":"alice"}),
                &caller,
            )?;
            let reopened = LocalState::open(&root.join("state"))?;
            let response = register_listener(
                &reopened,
                &client,
                "host",
                &json!({"about":"Work"}),
                &caller,
            )?;
            assert_eq!(response["title"], "alice");
            assert_eq!(
                reopened.custom_title("fixture", "default")?.as_deref(),
                Some("alice")
            );
            let remote = client
                .kv_get(string(&response, "canonical_listener_id"))?
                .ok_or("Missing renamed row")?
                .0;
            assert_eq!(remote["title"], "alice");
            assert_eq!(remote["custom_title"], true);
            let id = string(&response, "canonical_listener_id").to_string();
            reopened.db.execute("UPDATE local_listeners SET consecutive_probe_failures=2 WHERE hub=? AND listener_id=?", &[json!("work"),json!(id)])?;
            register_listener(
                &reopened,
                &client,
                "host",
                &json!({"about":"Work"}),
                &caller,
            )?;
            let row = reopened
                .listener("work", &id)?
                .ok_or("Missing idempotent binding")?;
            assert_eq!(
                row["consecutive_probe_failures"], 2,
                "Idempotent registration retains the current budget"
            );
            let plugin = crate::plugins::PluginDescriptor {
                id: "fixture".into(),
                kind: "agent-harness".into(),
                display_name: "Fixture".into(),
                tier: "experimental".into(),
                order: 0,
                sf_symbol: "terminal".into(),
                tint: "blue".into(),
                cli_flag: false,
                plugin_dir: root.clone(),
                is_builtin: false,
                command: vec![
                    "python3".into(),
                    "-c".into(),
                    "print(\'{\"ok\":true,\"status\":\"transient_error\"}\')".into(),
                ],
                channel_env: None,
                fields: vec![],
                post_setup_hint: None,
                post_disconnect_hint: None,
            };
            super::super::receiver::probe_listener(&reopened, &client, &row, &plugin, &root)?;
            let revoked = reopened
                .listener("work", &id)?
                .ok_or("Missing revoked binding")?;
            assert_eq!(revoked["consecutive_probe_failures"], 3);
            assert_eq!(revoked["status"], "UNLISTENED");
            register_listener(
                &reopened,
                &client,
                "host",
                &json!({"about":"Work"}),
                &caller,
            )?;
            let fresh = reopened
                .listener("work", &id)?
                .ok_or("Missing replacement binding")?;
            assert_eq!(fresh["consecutive_probe_failures"], 0);
            super::super::receiver::probe_listener(&reopened, &client, &fresh, &plugin, &root)?;
            let probed = reopened
                .listener("work", &id)?
                .ok_or("Missing probed replacement")?;
            assert_eq!(probed["consecutive_probe_failures"], 1);
            assert_eq!(probed["status"], "ACTIVE");
            // Hold an independent SQLite writer so revocation lands after the remote
            // CAS but before registration can acquire its local write transaction.
            let writer = super::super::database::Database::open(&root.join("state/state.sqlite"))?;
            let replacement = std::thread::scope(|scope| -> Result<Value, String> {
                let pending=writer.transaction(|connection| {
                    let pending=scope.spawn(||register_listener(&reopened,&client,"host",&json!({"about":"Concurrent refresh"}),&caller));
                    let deadline=std::time::Instant::now()+std::time::Duration::from_secs(3);
                    loop {
                        if client.kv_get(&id)?.is_some_and(|(remote,_)|remote["about"]=="Concurrent refresh") { break; }
                        if std::time::Instant::now()>=deadline { return Err("Registration did not publish its remote CAS".into()); }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    assert_eq!(connection.execute("UPDATE local_listeners SET status='UNLISTENED',updated_at=? WHERE hub=? AND listener_id=? AND status='ACTIVE' AND binding_generation=? AND owner_token=?", &[json!(now_ms()),json!("work"),json!(id),probed["binding_generation"].clone(),probed["owner_token"].clone()])?,1);
                    Ok(pending)
                })?;
                pending.join().map_err(|_| "Registration worker panicked")?
            })?;
            let final_row = reopened
                .listener("work", &id)?
                .ok_or("Missing transaction-fenced replacement")?;
            assert_eq!(final_row["status"], "ACTIVE");
            assert!(
                replacement["binding_generation"].as_u64() > probed["binding_generation"].as_u64()
            );
            assert_ne!(final_row["owner_token"], probed["owner_token"]);
            let before_detach = client.kv_get(&id)?;
            let stale = super::super::receiver::detach_channel_expected(
                &client,
                &id,
                "host:fixture",
                &probed,
            );
            assert!(stale.is_err_and(|error| error.starts_with("STALE_GENERATION:")));
            assert_eq!(
                client.kv_get(&id)?,
                before_detach,
                "An old revocation cannot detach the retried binding"
            );
            Ok(())
        })();
        engine.shutdown();
        fs::remove_dir_all(root).map_err(|e| e.to_string())?;
        result
    }

    #[test]
    fn automatic_second_host_preserves_remote_scope() -> Result<(), String> {
        for (about, title, user, expected) in [
            ("Session", "Human", "alice", None),
            ("Default", "Human", "alice", None),
            ("CAFÉ", "café", "alice", None),
            ("café", "CAFÉ", "alice", None),
            ("@ÉLODIE", "Human", " élodie ", None),
            ("you (@ÉLODIE)", "Human", "élodie", None),
            (" CAFÉ notes ", "café", "alice", Some("CAFÉ notes")),
        ] {
            assert_eq!(
                distinct_human_about(about, title, user),
                expected,
                "{about} / {title} / {user}"
            );
        }
        for (auth_mode, scope) in [("none", "self"), ("token", "workspace")] {
            let root = std::env::temp_dir().join(format!("cito-registration-{}", random_uuid_v4()));
            let engine = Arc::new(HubEngine::open(&root.join("hub"))?);
            engine.set_plugin_roots(&[])?;
            let result: Result<(), String> = (|| {
                let first = LocalState::open(&root.join("first"))?;
                let second = LocalState::open(&root.join("second"))?;
                let client = HubClient::new(
                    "work".into(),
                    json!({"url":"local://","authMode":auth_mode}),
                    "alice",
                    Some(engine.clone()),
                );
                let default = register_listener(
                    &first,
                    &client,
                    "first-host",
                    &json!({"about":"New session"}),
                    &json!({"harness":"fixture","session_id":"new","title":"Session"}),
                )?;
                let expected_scope = if auth_mode == "none" {
                    "workspace"
                } else {
                    "self"
                };
                assert_eq!(default["scope"], expected_scope);
                let id = string(&default, "canonical_listener_id");
                assert_eq!(
                    first.listener("work", id)?.ok_or("Missing default row")?["scope"],
                    expected_scope
                );
                assert_eq!(
                    client.kv_get(id)?.ok_or("Missing default Hub row")?.0["scope"],
                    expected_scope
                );
                let caller = json!({"harness":"human","session_id":"default","title":"Human"});
                let initial = register_listener(
                    &first,
                    &client,
                    "first-host",
                    &json!({"about":"Release approvals","scope":scope}),
                    &caller,
                )?;
                let id = string(&initial, "canonical_listener_id");
                let first_row = first
                    .listener("work", id)?
                    .ok_or("Missing first-host row")?;
                let (mut legacy, revision) = client.kv_get(id)?.ok_or("Missing registration")?;
                legacy["channels"]["first-host:human"]
                    .as_object_mut()
                    .ok_or("Missing first-host channel")?
                    .remove("owner_token");
                client.kv_save(id, &legacy, Some(revision))?;
                let attached = register_automatic_human(
                    &second,
                    &client,
                    "second-host",
                    &json!({"about":"Human receiver"}),
                    &caller,
                )?;
                assert_eq!(attached["canonical_listener_id"], id);
                assert_eq!(attached["scope"], scope);
                assert_eq!(attached["about"], "Release approvals");
                assert_eq!(
                    second
                        .listener("work", id)?
                        .ok_or("Missing fresh host row")?["about"],
                    "Release approvals"
                );
                assert_eq!(
                    attached["binding_generation"],
                    initial["binding_generation"]
                );
                assert_eq!(
                    second
                        .listener("work", id)?
                        .ok_or("Missing second-host row")?["scope"],
                    scope
                );
                let remote = client.kv_get(id)?.ok_or("Missing remote registration")?.0;
                assert_eq!(remote["scope"], scope);
                assert_eq!(remote["about"], "Release approvals");
                assert_eq!(remote["channels"]["first-host:human"]["state"], "ACTIVE");
                assert_eq!(remote["channels"]["second-host:human"]["state"], "ACTIVE");
                let second_row = second
                    .listener("work", id)?
                    .ok_or("Missing second-host row")?;
                assert_eq!(remote["owner_token"], legacy["owner_token"]);
                assert_ne!(first_row["owner_token"], second_row["owner_token"]);
                assert!(remote_matches_listener(
                    &remote,
                    &first_row,
                    "first-host:human"
                ));
                assert!(remote_matches_listener(
                    &remote,
                    &second_row,
                    "second-host:human"
                ));
                for about in [
                    "Human",
                    "Human receiver",
                    "Human Node",
                    "Human channel",
                    "Session",
                    "Coding session",
                    "Default",
                    "Agent",
                    "Agent session",
                    "CLI session",
                    "alice",
                    "@alice",
                    " hUmAn NoDe ",
                    "You",
                    "You (@alice)",
                    " \n",
                ] {
                    register_automatic_human(
                        &second,
                        &client,
                        "second-host",
                        &json!({"about":about}),
                        &caller,
                    )?;
                    let refreshed = second
                        .listener("work", id)?
                        .ok_or("Missing refreshed row")?;
                    assert_eq!(
                        refreshed["binding_generation"],
                        second_row["binding_generation"]
                    );
                    assert_eq!(refreshed["owner_token"], second_row["owner_token"]);
                    assert_eq!(refreshed["about"], "Release approvals", "{about}");
                    assert_eq!(
                        client.kv_get(id)?.ok_or("Missing remote about")?.0["about"],
                        "Release approvals",
                        "{about}"
                    );
                }
                let (mut reg, rev) = client.kv_get(id)?.ok_or("Missing registration")?;
                reg["channels"]["hub:fixture"] =
                    json!({"host_id":"hub","state":"ACTIVE","updated_at_ms":0});
                client.kv_save(id, &reg, Some(rev))?;
                super::super::receiver::detach_channel_expected(
                    &client,
                    id,
                    "first-host:human",
                    &first_row,
                )?;
                let paused = client.kv_get(id)?.ok_or("Missing paused registration")?.0;
                assert_eq!(paused["human_paused"], true);
                assert_eq!(paused["state"], "UNLISTENED");
                assert_eq!(paused["channels"]["second-host:human"]["state"], "ACTIVE");
                assert!(register_automatic_human(
                    &second,
                    &client,
                    "second-host",
                    &json!({"about":"Human"}),
                    &caller
                )
                .is_err());
                register_listener(
                    &second,
                    &client,
                    "second-host",
                    &json!({"about":"Human"}),
                    &caller,
                )?;
                let resumed = client.kv_get(id)?.ok_or("Missing resumed registration")?.0;
                assert_eq!(resumed["human_paused"], false);
                assert_eq!(resumed["state"], "ACTIVE");
                let after_resume = second
                    .listener("work", id)?
                    .ok_or("Missing resumed local row")?;
                assert_ne!(after_resume["owner_token"], second_row["owner_token"]);
                assert!(
                    !second.pause_listener_for(&second_row)?,
                    "A delayed heartbeat must not undo an explicit resume"
                );
                assert_eq!(
                    second
                        .listener("work", id)?
                        .ok_or("Missing resumed local row")?["status"],
                    "ACTIVE"
                );
                assert_eq!(
                    resumed["channels"]["hub:fixture"],
                    reg["channels"]["hub:fixture"]
                );
                assert!(first.pause_listener_for(&first_row)?);
                first.db.execute(
                    "INSERT INTO pending_hub_detaches(hub,listener_id,hub_url) VALUES(?,?,?)",
                    &[json!("work"), json!(id), client.config["url"].clone()],
                )?;
                assert!(
                    register_automatic_human(
                        &first,
                        &client,
                        "first-host",
                        &json!({"about":"Human"}),
                        &caller
                    )
                    .is_err(),
                    "An unsettled local pause must win over a remote active sibling"
                );
                first.db.execute(
                    "DELETE FROM pending_hub_detaches WHERE hub=? AND listener_id=?",
                    &[json!("work"), json!(id)],
                )?;
                first.db.execute(
                    "UPDATE local_listeners SET about=? WHERE hub=? AND listener_id=?",
                    &[json!(" Release approvals "), json!("work"), json!(id)],
                )?;
                let recovered = register_automatic_human(
                    &first,
                    &client,
                    "first-host",
                    &json!({"about":"Human"}),
                    &caller,
                )?;
                assert_eq!(recovered["scope"], scope);
                assert_eq!(recovered["about"], "Release approvals");
                let mut first_resumed = first
                    .listener("work", id)?
                    .ok_or("Missing automatically resumed Human")?;
                assert_eq!(first_resumed["status"], "ACTIVE");
                assert_ne!(first_resumed["owner_token"], first_row["owner_token"]);
                assert!(
                    !first.pause_listener_for(&first_row)?,
                    "An old heartbeat cannot undo cross-host resume"
                );
                // Generic local metadata must not erase distinct metadata saved remotely.
                for (local_about, remote_about, expected) in [
                    (" \n", "Remote approvals", "Remote approvals"),
                    ("Human Node", "Release approvals", "Release approvals"),
                    (" hUmAn NoDe ", "Release approvals", "Release approvals"),
                    ("You", "Release approvals", "Release approvals"),
                    ("Human receiver", "You", "Human receiver"),
                    ("Human", "Human Node", "Human receiver"),
                    ("You (@alice)", "Human", "Human receiver"),
                ] {
                    assert!(first.pause_listener_for(&first_resumed)?);
                    first.db.execute(
                        "UPDATE local_listeners SET about=? WHERE hub=? AND listener_id=?",
                        &[json!(local_about), json!("work"), json!(id)],
                    )?;
                    let (mut remote, revision) = client.kv_get(id)?.ok_or("Missing metadata")?;
                    remote["about"] = json!(remote_about);
                    client.kv_save(id, &remote, Some(revision))?;
                    let recovered = register_automatic_human(
                        &first,
                        &client,
                        "first-host",
                        &json!({"about":"Human receiver"}),
                        &caller,
                    )?;
                    assert_eq!(recovered["about"], expected);
                    assert_eq!(
                        first.listener("work", id)?.ok_or("Missing saved about")?["about"],
                        expected
                    );
                    // Keep the captured lease current for the next pause and sibling test.
                    first_resumed = first.listener("work", id)?.ok_or("Missing resumed row")?;
                    assert_eq!(first_resumed["scope"], scope);
                }
                let (mut remote, revision) = client.kv_get(id)?.ok_or("Missing active Human")?;
                remote["channels"]["second-host:human"]["state"] = json!("UNLISTENED");
                remote["channels"]["hub:fixture"]["state"] = json!("UNLISTENED");
                client.kv_save(id, &remote, Some(revision))?;
                assert!(first.pause_listener_for(&first_resumed)?);
                assert!(
                    register_automatic_human(
                        &first,
                        &client,
                        "first-host",
                        &json!({"about":"Human"}),
                        &caller
                    )
                    .is_err(),
                    "An inactive remote sibling is not a resume"
                );
                Ok(())
            })();
            engine.shutdown();
            fs::remove_dir_all(root).map_err(|e| e.to_string())?;
            result?;
        }
        Ok(())
    }
}
