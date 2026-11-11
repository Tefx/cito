//! Shared registration serializes background enrollment with explicit local intent.
use super::{
    auth::principal_ids_equal,
    identity::{listener_id, listener_id_candidates, local_host_name, public_listener_id, string},
    state::LocalState,
    transport::HubClient,
};
use crate::plugins::{now_ms, random_uuid_v4};
use serde_json::{json, Value};

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
    if local_node(state, client, "human", "default")?
        .is_some_and(|row| row["status"] == "UNLISTENED")
    {
        return Err("BINDING_REQUIRED: Human enrollment was explicitly disabled".into());
    }
    register_locked(state, client, host_id, args, caller, false)
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
    let title = match string(caller, "title") {
        title if harness == "human" && title.trim().is_empty() => "Human",
        title => title,
    };
    if harness.is_empty() || session.is_empty() || (harness == "human" && session != "default") {
        return Err("INVALID_INPUT: Invalid Node session identity".into());
    }
    let local = local_node(state, client, harness, session)?;
    let mut id = local
        .as_ref()
        .map(|row| string(row, "listener_id").to_string())
        .unwrap_or_else(|| listener_id(string(&client.identity, "principal_id"), harness, session));
    let mut initial_remote = client.kv_get(&id)?;
    if local.is_none() && initial_remote.is_none() {
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
        .or_else(|| local.as_ref().map(|row| row["tag"].clone()))
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
        state.db.transaction(|connection| {
            // Close precedes profile fencing under this same database mutex.
            if client.closed.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("NATS_UNAVAILABLE: Hub disconnected during registration".into());
            }
            let changed = connection.execute(
                "INSERT INTO local_listeners(hub,listener_id,public_id,tag,principal_id,owner_user,host_id,host_name,harness,session_id,endpoint_pid,cwd,repo,branch,title,about,scope,status,binding_generation,owner_token) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'ACTIVE',?,?) ON CONFLICT(hub,listener_id) DO UPDATE SET public_id=excluded.public_id,tag=excluded.tag,owner_user=excluded.owner_user,host_id=excluded.host_id,host_name=excluded.host_name,endpoint_pid=excluded.endpoint_pid,cwd=excluded.cwd,repo=excluded.repo,branch=excluded.branch,title=excluded.title,about=excluded.about,scope=excluded.scope,status='ACTIVE',binding_generation=excluded.binding_generation,owner_token=excluded.owner_token WHERE local_listeners.status!='ACTIVE' OR local_listeners.binding_generation<=excluded.binding_generation",
                &[json!(hub),json!(id),json!(public),tag.clone(),client.identity["principal_id"].clone(),client.identity["owner_user"].clone(),json!(host_id),json!(host_name),json!(harness),json!(session),caller["endpoint_pid"].clone(),json!(string(caller,"cwd")),caller["repo"].clone(),caller["branch"].clone(),json!(title),json!(about),json!(scope),json!(generation),json!(owner)],
            )?;
            if changed == 0 { return Err("STALE_GENERATION: Local binding was replaced".into()); }
            Ok(())
        })?;
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
            Ok(())
        })();
        engine.shutdown();
        fs::remove_dir_all(root).map_err(|e| e.to_string())?;
        result
    }

    #[test]
    fn automatic_second_host_preserves_remote_scope() -> Result<(), String> {
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
                let caller = json!({"harness":"human","session_id":"default","title":"Human"});
                let initial = register_listener(
                    &first,
                    &client,
                    "first-host",
                    &json!({"about":"Human","scope":scope}),
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
                    &json!({"about":"Human"}),
                    &caller,
                )?;
                assert_eq!(attached["canonical_listener_id"], id);
                assert_eq!(attached["scope"], scope);
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
                register_automatic_human(
                    &second,
                    &client,
                    "second-host",
                    &json!({"about":"Human"}),
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
                assert_eq!(
                    resumed["channels"]["hub:fixture"],
                    reg["channels"]["hub:fixture"]
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
