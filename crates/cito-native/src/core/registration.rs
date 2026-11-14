//! Shared registration serializes background enrollment with explicit local intent.
use super::{
    auth::{principal_ids_equal, principal_matches},
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
    let registration = state.registration_lock(&client.name)?;
    let _registration = registration
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
        "SELECT * FROM local_listeners WHERE hub=? AND harness=? AND session_id=? AND status!='FENCED_INACTIVE' ORDER BY status='ACTIVE' DESC,binding_generation DESC",
        &[json!(client.name), json!(harness), json!(session)],
    )?.into_iter().find(|row| principal_matches(string(row,"principal_id"), &client.identity)))
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
    (if remote["harness"] == "human" {
        &remote["channels"][channel]["binding_generation"]
    } else {
        &remote["binding_generation"]
    }) == &row["binding_generation"]
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
    let registration = state.registration_lock(&client.name)?;
    let _registration = registration
        .lock()
        .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
    register_locked(state, client, host_id, args, caller)
}

pub(super) fn is_generic_session_label(label: &str, user: &str, home_basename: &str) -> bool {
    let label = label.trim().to_lowercase();
    let identity = label.strip_prefix('@').unwrap_or(&label);
    let user = user.trim().to_lowercase();
    let home = home_basename.trim().to_lowercase();
    label.is_empty()
        || identity == user
        || identity == home
        || label == format!("you (@{user})")
        || label == format!("@{user} (me)")
        || [
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
        .contains(&label.as_str())
}

pub(super) fn distinct_label<'a>(
    label: &'a str,
    title: &str,
    user: &str,
    home: &str,
) -> Option<&'a str> {
    let label = label.trim();
    (!is_generic_session_label(label, user, home)
        && label.to_lowercase() != title.trim().to_lowercase())
    .then_some(label)
}

pub fn register_automatic_human(
    state: &LocalState,
    client: &HubClient,
    host_id: &str,
    args: &Value,
    caller: &Value,
) -> Result<Value, String> {
    let registration = state.registration_lock(&client.name)?;
    let _registration = registration
        .lock()
        .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
    if local_node(state, client, "human", "default")?
        .is_some_and(|row| row["status"] == "UNLISTENED")
    {
        return Err("BINDING_REQUIRED: Device enrollment was explicitly disabled".into());
    }
    register_locked(state, client, host_id, args, caller)
}

fn register_human_device(
    state: &LocalState,
    client: &HubClient,
    host: &str,
    caller: &Value,
) -> Result<Value, String> {
    let previous = local_node(state, client, "human", "default")?;
    // The Hub owns Human identity, including retained keys and verified aliases.
    let remote = client
        .directory()?
        .into_iter()
        .find(|record| {
            record["harness"] == "human"
                && record["session_id"] == "default"
                && principal_matches(string(record, "principal_id"), &client.identity)
        })
        .unwrap_or(Value::Null);
    let channel = format!("{host}:human");
    let reuse = previous
        .as_ref()
        .filter(|r| r["status"] == "ACTIVE" && remote_matches_listener(&remote, r, &channel));
    let generation = if let Some(row) = reuse {
        row["binding_generation"].as_u64().unwrap_or(1)
    } else {
        previous
            .as_ref()
            .and_then(|r| r["binding_generation"].as_u64())
            .unwrap_or(0)
            .max(
                remote["channels"][&channel]["binding_generation"]
                    .as_u64()
                    .unwrap_or(0),
            )
            .checked_add(1)
            .filter(|g| *g <= i64::MAX as u64)
            .ok_or("STALE_GENERATION: Device generation exhausted")?
    };
    let token = reuse
        .map(|r| string(r, "owner_token").to_owned())
        .unwrap_or_else(random_uuid_v4);
    let result = client.human_request("human_device_attach", &json!({"channel_id":channel,"host_id":host,"host_name":local_host_name(),"binding_generation":generation,"owner_token":token}))?;
    let record = &result["record"];
    let id = record["listener_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or("STATE_ERROR: Hub returned no Human identity")?;
    let public = record["public_id"].clone();
    state.db.transaction(|c| {
        if client.closed.load(std::sync::atomic::Ordering::SeqCst) { return Err("NATS_UNAVAILABLE: Hub disconnected during enrollment".into()); }
        c.execute("INSERT INTO local_listeners(hub,listener_id,public_id,principal_id,owner_user,host_id,host_name,harness,session_id,cwd,title,about,scope,status,binding_generation,owner_token,hub_url,updated_at) VALUES(?,?,?,?,?,?,?,'human','default','','Human',?,?,'ACTIVE',?,?,?,?) ON CONFLICT(hub,listener_id) DO UPDATE SET public_id=excluded.public_id,principal_id=excluded.principal_id,owner_user=excluded.owner_user,host_id=excluded.host_id,host_name=excluded.host_name,about=excluded.about,scope=excluded.scope,status='ACTIVE',binding_generation=excluded.binding_generation,owner_token=excluded.owner_token,hub_url=excluded.hub_url,updated_at=excluded.updated_at", &[json!(client.name),json!(id),public.clone(),client.identity["principal_id"].clone(),client.identity["owner_user"].clone(),json!(host),json!(local_host_name()),record["about"].clone(),record["scope"].clone(),json!(generation),json!(token),client.config["url"].clone(),json!(now_ms())])?;
        c.execute("DELETE FROM pending_hub_detaches WHERE hub=? AND listener_id=?", &[json!(client.name),json!(id)])?;
        Ok(())
    })?;
    let _ = caller;
    Ok(
        json!({"ok":true,"status":"listening","hub":client.name,"listener_id":public,"canonical_listener_id":id,"binding_generation":generation,"harness":"human","session_id":"default","title":record["title"],"scope":record["scope"],"about":record["about"]}),
    )
}

pub(super) const DISCOVERY_CUTOFF_SQL: &str =
    "SELECT COALESCE(MAX(updated_at),0) AS revoked_at FROM local_listeners WHERE harness=?1 AND session_id=?2 AND (status='UNLISTENED' OR (status='ACTIVE' AND endpoint_pid>0 AND ?3>0 AND endpoint_pid!=?3))";

enum RegistrationCommit {
    Committed,
    Retry,
    StaleDiscovery,
}

fn register_locked(
    state: &LocalState,
    client: &HubClient,
    host_id: &str,
    args: &Value,
    caller: &Value,
) -> Result<Value, String> {
    let hub = &client.name;
    let discovered_at = args["discovered_at"].as_u64().unwrap_or(0);
    let candidate_pid = caller["endpoint_pid"].as_u64().unwrap_or(0);
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
    if harness == "human" {
        return register_human_device(state, client, host_id, caller);
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
        .unwrap_or_else(|| string(caller, "title"));
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
        let sibling = previous["channels"].as_object().is_some_and(|channels| {
            channels
                .iter()
                .any(|(key, value)| key != &channel && value["state"] == "ACTIVE")
        });
        let same_binding = local.as_ref().filter(|row| {
            row["status"] == "ACTIVE"
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
        channels.insert(channel.clone(),json!({"channel_id":channel,"kind":harness,"host_id":host_id,"host_name":host_name,"delivery_mode":"wake_exclusive","priority":100,"state":"ACTIVE","owner_token":owner,"updated_at_ms":client.server_now()}));
        let mut value = json!({"listener_id":id,"public_id":public,"tag":tag,"principal_id":client.identity["principal_id"],"owner_user":client.identity["owner_user"],"host_id":host_id,"host_name":host_name,"harness":harness,"session_id":session,"cwd":string(caller,"cwd"),"repo":caller["repo"],"branch":caller["branch"],"title":title,"about":about,"scope":scope,"state":"ACTIVE","binding_generation":generation,"owner_token":node_owner,"channels":channels});
        value["custom_title"] = json!(custom_title.is_some());
        super::lease::reconcile_registration_state(&mut value, client.server_now());
        if discovered_at > 0
            && state
                .db
                .query(
                    DISCOVERY_CUTOFF_SQL,
                    &[json!(harness), json!(session), json!(candidate_pid)],
                )?
                .first()
                .and_then(|row| row["revoked_at"].as_u64())
                .is_some_and(|revoked| revoked > 0 && discovered_at <= revoked)
        {
            return Err("STALE_DISCOVERY: Candidate was revoked before registration".into());
        }
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
            // Admission and cross-Hub revocation share the same SQLite write lock.
            let cutoff = if discovered_at > 0 { connection.query(
                DISCOVERY_CUTOFF_SQL, &[json!(harness),json!(session),json!(candidate_pid)],
            )?.first().and_then(|row|row["revoked_at"].as_u64()).unwrap_or(0) } else { 0 };
            let stale = cutoff > 0 && discovered_at <= cutoff;
            // The remote CAS cannot authorize reuse after a concurrent local revocation.
            if !stale && same_binding.is_some() && connection.query(
                "SELECT 1 FROM local_listeners WHERE hub=? AND listener_id=? AND status='ACTIVE' AND binding_generation=? AND COALESCE(owner_token,'')=?",
                &[json!(hub), json!(id), json!(generation), json!(owner)],
            )?.is_empty() { return Ok(RegistrationCommit::Retry); }
            let changed = connection.execute(
                "INSERT INTO local_listeners(hub,listener_id,public_id,tag,principal_id,owner_user,host_id,host_name,harness,session_id,endpoint_pid,cwd,repo,branch,title,about,scope,status,binding_generation,owner_token,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(hub,listener_id) DO UPDATE SET consecutive_probe_failures=CASE WHEN status!='ACTIVE' OR owner_token!=excluded.owner_token OR COALESCE(endpoint_pid,-1)!=COALESCE(excluded.endpoint_pid,-1) THEN 0 ELSE consecutive_probe_failures END,public_id=excluded.public_id,tag=excluded.tag,owner_user=excluded.owner_user,host_id=excluded.host_id,host_name=excluded.host_name,endpoint_pid=excluded.endpoint_pid,cwd=excluded.cwd,repo=excluded.repo,branch=excluded.branch,title=excluded.title,about=excluded.about,scope=excluded.scope,status=excluded.status,binding_generation=excluded.binding_generation,owner_token=excluded.owner_token,updated_at=MAX(local_listeners.updated_at,excluded.updated_at) WHERE local_listeners.status!='ACTIVE' OR local_listeners.binding_generation<=excluded.binding_generation",
                &[json!(hub),json!(id),json!(public),tag.clone(),client.identity["principal_id"].clone(),client.identity["owner_user"].clone(),json!(host_id),json!(host_name),json!(harness),json!(session),caller["endpoint_pid"].clone(),json!(string(caller,"cwd")),caller["repo"].clone(),caller["branch"].clone(),json!(title),json!(about),json!(scope),json!(if stale { "UNLISTENED" } else { "ACTIVE" }),json!(generation),json!(owner),json!(now_ms().max(cutoff))],
            )?;
            if changed == 0 { return Err("STALE_GENERATION: Local binding was replaced".into()); }
            connection.execute("UPDATE local_listeners SET hub_url=? WHERE hub=? AND listener_id=?", &[client.config["url"].clone(),json!(hub),json!(id)])?;
            if stale {
                // Retain both the published owner and retry intent before any cleanup IO.
                connection.execute("INSERT INTO pending_hub_detaches(hub,listener_id,hub_url) VALUES(?,?,?) ON CONFLICT(hub,listener_id) DO UPDATE SET hub_url=excluded.hub_url", &[json!(hub),json!(id),client.config["url"].clone()])?;
                Ok(RegistrationCommit::StaleDiscovery)
            } else {
                connection.execute("DELETE FROM pending_hub_detaches WHERE hub=? AND listener_id=?", &[json!(hub),json!(id)])?;
                Ok(RegistrationCommit::Committed)
            }
        })?;
        match committed {
            RegistrationCommit::Retry => continue,
            RegistrationCommit::StaleDiscovery => {
                // Drop the database lock before IO; fence cleanup to this channel's token.
                state.invalidate_discovery(harness)?;
                if super::receiver::detach_channel_expected(
                    client,
                    &id,
                    &channel,
                    &json!({"binding_generation":generation,"owner_token":owner}),
                )
                .is_ok()
                {
                    state.clear_pending_detach_if_current(hub, &id, generation, &owner)?;
                }
                return Err("STALE_DISCOVERY: Candidate was revoked during registration".into());
            }
            RegistrationCommit::Committed => {
                if discovered_at == 0
                    || local
                        .as_ref()
                        .is_some_and(|row| row["endpoint_pid"] != caller["endpoint_pid"])
                {
                    state.invalidate_discovery(harness)?;
                }
            }
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
    fn local_node_selects_current_principal_and_ignores_fenced_history() -> Result<(), String> {
        let root = std::env::temp_dir().join(format!("cito-registration-{}", random_uuid_v4()));
        let result = (|| {
            let state = LocalState::open(&root)?;
            let client = HubClient::new("work".into(), json!({"url":"local://"}), "alice", None);
            for (id, status, generation, principal) in [
                ("c_1111111111111111", "UNLISTENED", 1, "none#alice"),
                ("c_2222222222222222", "UNLISTENED", 2, "none#alice"),
                ("c_3333333333333333", "FENCED_INACTIVE", 99, "none#alice"),
                ("c_4444444444444444", "ACTIVE", 100, "none#bob"),
            ] {
                state.db.execute("INSERT INTO local_listeners(hub,listener_id,principal_id,owner_user,host_id,host_name,harness,session_id,cwd,title,about,scope,status,binding_generation) VALUES('work',?,?,'alice','host','host','human','default','','Human','Review approvals','self',?,?)",
                    &[json!(id), json!(principal), json!(status), json!(generation)])?;
            }
            let row =
                local_node(&state, &client, "human", "default")?.ok_or("Missing paused row")?;
            assert_eq!(row["listener_id"], "c_2222222222222222");
            assert_eq!(row["status"], "UNLISTENED");
            state.db.execute("UPDATE local_listeners SET status='FENCED_INACTIVE' WHERE principal_id='none#alice'", &[])?;
            assert!(local_node(&state, &client, "human", "default")?.is_none());
            for (label, generic) in [
                (" @ÉLODIE ", true),
                ("You (@ÉLODIE)", true),
                (" @ÉLODIE (Me) ", true),
                ("@bob (Me)", false),
                ("Élodie notes", false),
                (" default ", true),
            ] {
                assert_eq!(is_generic_session_label(label, "élodie", "home"), generic);
            }
            assert_eq!(distinct_label(" CAFÉ ", "café", "alice", "home"), None);
            assert_eq!(distinct_label(" @BOB (mE) ", "", " Bob ", "home"), None);
            assert_eq!(
                distinct_label("Bob reviews", "", "bob", "home"),
                Some("Bob reviews")
            );
            Ok(())
        })();
        fs::remove_dir_all(root).map_err(|e| e.to_string())?;
        result
    }

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
                ("human", json!("Review desk"), "Human"),
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
        let root = std::env::temp_dir().join(format!("cito-human-devices-{}", random_uuid_v4()));
        let engine = Arc::new(HubEngine::open(&root.join("hub"))?);
        engine.set_plugin_roots(&[])?;
        let result = (|| {
            let first = LocalState::open(&root.join("first"))?;
            let second = LocalState::open(&root.join("second"))?;
            let client = HubClient::new(
                "work".into(),
                json!({"url":"local://"}),
                "alice",
                Some(engine.clone()),
            );
            let caller = json!({"harness":"human","session_id":"default","title":"Human"});
            let initial = register_listener(
                &first,
                &client,
                "first-host",
                &json!({"about":"Ignored device description"}),
                &caller,
            )?;
            let id = string(&initial, "canonical_listener_id");
            client.human_request("human_inbox_update",&json!({"node_id":id,"expected_control_revision":1,"paused":true,"scope":"self","about":"Release approvals"}))?;
            let attached = register_automatic_human(
                &second,
                &client,
                "second-host",
                &json!({"about":"Must not overwrite","scope":"workspace"}),
                &caller,
            )?;
            assert_eq!(attached["scope"], "self");
            assert_eq!(attached["about"], "Release approvals");
            let record = client.kv_get(id)?.ok_or("Missing Human")?.0;
            assert_eq!(record["human_paused"], true);
            assert_eq!(record["human_control_revision"], 2);
            let first_row = first.listener("work", id)?.ok_or("Missing device")?;
            super::super::receiver::detach_channel_expected(
                &client,
                id,
                "first-host:human",
                &first_row,
            )?;
            assert!(first.pause_listener_for(&first_row)?);
            client.human_request(
                "human_inbox_update",
                &json!({"node_id":id,"expected_control_revision":2,"paused":false}),
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
                "A shared Resume must not reattach an explicitly detached device"
            );
            let resumed = client.kv_get(id)?.ok_or("Missing Human")?.0;
            assert_eq!(resumed["about"], "Release approvals");
            assert_eq!(
                resumed["channels"]["first-host:human"]["state"],
                "UNLISTENED"
            );
            assert_eq!(resumed["channels"]["second-host:human"]["state"], "ACTIVE");
            register_listener(
                &first,
                &client,
                "first-host",
                &json!({"about":"Human"}),
                &caller,
            )?;
            let final_record = client.kv_get(id)?.ok_or("Missing Human")?.0;
            assert_eq!(final_record["human_control_revision"], 3);
            assert_eq!(final_record["scope"], "self");
            assert_eq!(
                final_record["channels"]["first-host:human"]["binding_generation"],
                2
            );
            assert_eq!(
                final_record["channels"]["second-host:human"]["binding_generation"],
                1
            );
            client.rpc("js_publish", &json!({"subject":format!("cito.signal.{id}"),"data":{"signal_id":"after-reattach","to":id,"target_generation":final_record["binding_generation"],"from":{"principal_id":client.identity["principal_id"],"user":"alice","host_name":"desktop","title":"Sender"},"message":"Fresh Human message"}}))?;
            for state in [&first, &second] {
                let row = state.listener("work", id)?.ok_or("Missing local Human")?;
                super::super::receiver::poll_listener(state, &client, &row, None, &root, |_| {})?;
                let signals = state.db.query(
                    "SELECT state,held_reason FROM local_signals WHERE signal_id='after-reattach'",
                    &[],
                )?;
                assert_eq!(signals.len(), 1);
                assert_eq!(signals[0]["state"], "RECEIVED");
                assert!(signals[0]["held_reason"].is_null());
            }
            assert_eq!(
                final_record["channels"]["first-host:human"]["state"],
                "ACTIVE"
            );
            Ok(())
        })();
        engine.shutdown();
        fs::remove_dir_all(root).map_err(|e| e.to_string())?;
        result
    }
}
