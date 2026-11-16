use super::{
    identity::{iso, string},
    registration::remote_matches_listener,
    state::LocalState,
    transport::HubClient,
};
use crate::plugins::{now_ms, PluginDescriptor};
use serde_json::{json, Value};
use std::time::Duration;

pub fn plugin_request(
    plugin: &PluginDescriptor,
    request: &Value,
    renew: Option<(&HubClient, &Value)>,
    fence: Option<(&LocalState, &Value)>,
) -> Result<Value, String> {
    plugin_request_owned(plugin, request, renew, fence, Duration::from_secs(10))
        .map_err(|e| e.message)
}

#[derive(Clone, Copy, PartialEq)]
enum ClaimLoss {
    AlreadyAcked,
    AlreadyClaimed,
}
struct WakeFailure {
    message: String,
    claim_loss: Option<ClaimLoss>,
}
fn plugin_request_owned(
    plugin: &PluginDescriptor,
    request: &Value,
    renew: Option<(&HubClient, &Value)>,
    fence: Option<(&LocalState, &Value)>,
    renewal_interval: Duration,
) -> Result<Value, WakeFailure> {
    let mut claim_loss = None;
    let mut last_renew = Duration::ZERO;
    let mut checked_remote = false;
    let mut progress =
        |elapsed: Duration| {
            if super::daemon::shutdown_requested() {
                return Err("Daemon is shutting down".into());
            }
            if let Some((client, _)) = renew {
                if client.closed.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err("NATS_UNAVAILABLE: Hub disconnected during wake".into());
                }
            }
            if let Some((state, row)) = fence {
                let current = state
                    .listener(string(row, "hub"), string(row, "listener_id"))?
                    .ok_or("STALE_GENERATION: Listener removed")?;
                if current["status"] != "ACTIVE"
                    || current["binding_generation"] != row["binding_generation"]
                    || current["owner_token"] != row["owner_token"]
                {
                    return Err("STALE_GENERATION: Listener changed during wake".into());
                }
                if let Some(batch) = row.get("wake_batch") {
                    state.check_dispatch_in_flight(row, batch, false)?;
                }
            }
            let renewal_due = elapsed.saturating_sub(last_renew) >= renewal_interval;
            if !checked_remote || renewal_due {
                if let (Some((client, claim)), Some((state, row))) = (renew, fence) {
                    let channel = string(claim, "channel_id");
                    let Some((mut remote, revision)) =
                        client.kv_get(string(row, "listener_id"))?.filter(|(r, _)| {
                            r.is_object()
                                && r["state"] == "ACTIVE"
                                && remote_matches_listener(r, row, channel)
                                && r["channels"][channel]["state"] == "ACTIVE"
                        })
                    else {
                        state.fence_listener_for(row, "fenced_before_dispatch")?;
                        return Err("STALE_GENERATION: Remote listener changed during wake".into());
                    };
                    if renewal_due {
                        if let Some(ch) = remote["channels"].get_mut(channel) {
                            let now = client.server_now();
                            ch.as_object_mut()
                                .ok_or("INVALID_INPUT: Listener channel must be an object")?
                                .insert("updated_at_ms".into(), json!(now));
                            remote["updated_at_ms"] = json!(now);
                            client.kv_save(string(row, "listener_id"), &remote, Some(revision))?;
                        }
                    }
                }
                checked_remote = true;
            }
            if renewal_due {
                if let Some((client, claim)) = renew {
                    match client.rpc("signal_claim_dispatch", claim) {
                        Ok(v) if v["claimed"] == true => {}
                        Ok(v) => {
                            if let (Some((state, row)), Some(acked)) =
                                (fence, v["acked_ids"].as_array())
                            {
                                state.acknowledge(
                                    string(row, "hub"),
                                    string(row, "listener_id"),
                                    acked,
                                    string(claim, "channel_id"),
                                )?;
                            }
                            claim_loss = Some(if v["reason"] == "already_acked" {
                                ClaimLoss::AlreadyAcked
                            } else {
                                ClaimLoss::AlreadyClaimed
                            });
                            return Err(
                            "Wake lease renewal lost; delivery is at-least-once under partition"
                                .into(),
                        );
                        }
                        Err(_) => return Err(
                            "Wake lease renewal failed; delivery is at-least-once under partition"
                                .into(),
                        ),
                    }
                }
                if let Some((state, row)) = fence {
                    if let Some(batch) = row.get("wake_batch") {
                        state.check_dispatch_in_flight(row, batch, true)?;
                    }
                }
                last_renew = elapsed;
            }
            Ok(())
        };
    // Validate ownership before spawning, as well as before every pipe operation.
    progress(Duration::ZERO).map_err(|message| WakeFailure {
        message,
        claim_loss: None,
    })?;
    if let Some((state, row)) = fence {
        if let Some(batch) = row.get("wake_batch") {
            state
                .check_dispatch(row, batch, false)
                .map_err(|message| WakeFailure {
                    message,
                    claim_loss: None,
                })?;
        }
    }
    let result = crate::plugins::plugin_request_with_progress(
        plugin,
        request,
        // Wake completion can outlive the ordinary one-shot budget. Renew the
        // 30-second Hub claim every 10 seconds while this child remains owned.
        if renew.is_some() {
            Duration::from_secs(120)
        } else {
            Duration::from_secs(10)
        },
        progress,
    );
    result.map_err(|message| WakeFailure {
        message,
        claim_loss,
    })
}

pub fn poll_listener(
    local: &LocalState,
    client: &HubClient,
    row: &Value,
    plugin: Option<&PluginDescriptor>,
    state_dir: &std::path::Path,
    mut on_admit: impl FnMut(&Value),
) -> Result<(), String> {
    let id = string(row, "listener_id");
    let hub = &client.name;
    if row["hub_url"] != client.config["url"]
        || !super::auth::principal_ids_equal(
            string(row, "principal_id"),
            string(&client.identity, "principal_id"),
        )
    {
        return Err("STALE_GENERATION: Listener authority changed".into());
    }
    local.isolate_stale_signals(row)?;
    let name = format!("{}_{}", string(row, "host_id"), id);
    let added=client.rpc("consumer_add",&json!({"stream":"CITO_SIGNALS","config":{"durable_name":name,"filter_subject":format!("cito.signal.{id}"),"deliver_policy":"all"}}))?;
    // A fallback response keeps its own clock observation, including time spent
    // waiting for a failing backlog request.
    let added_observed = std::time::Instant::now();
    let (backlog, observed) = match client.rpc("consumer_backlog", &json!({"name":name})) {
        Ok(backlog) => (backlog, std::time::Instant::now()),
        Err(_) => (added, added_observed),
    };
    let (hub_now, observed) = match backlog["server_now_ms"].as_u64() {
        Some(now) => (now, observed),
        None => (client.server_now(), std::time::Instant::now()),
    };
    let registration = local.registration_lock(hub)?;
    let registration_guard = if row["harness"] == "human" {
        let guard = registration
            .lock()
            .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
        // Read policy after the backlog: a remote scope promotion may have enabled
        // these very signals. A pre-backlog snapshot can silently drop and ack them.
        let (remote, _) = client
            .kv_get(id)?
            .ok_or("LISTENER_NOT_FOUND: Human inbox missing")?;
        let channel = format!("{}:human", string(row, "host_id"));
        if !remote_matches_listener(&remote, row, &channel)
            || remote["channels"][&channel]["state"] != "ACTIVE"
        {
            local.fence_listener_for(row, "fenced_before_dispatch")?;
            return Err("STALE_GENERATION: Human attachment superseded".into());
        }
        super::daemon::DaemonService::reconcile_human_scope(
            local,
            client,
            string(row, "host_id"),
            &remote,
        )?;
        Some(guard)
    } else {
        let remote = client
            .kv_get(id)?
            .ok_or("LISTENER_NOT_FOUND: Listener missing")?
            .0;
        let channel = format!("{}:{}", string(row, "host_id"), string(row, "harness"));
        if !remote_matches_listener(&remote, row, &channel)
            || remote["channels"][&channel]["state"] != "ACTIVE"
        {
            local.fence_listener_for(row, "fenced_before_dispatch")?;
            return Err("STALE_GENERATION: Listener attachment superseded".into());
        }
        None
    };
    for item in backlog["backlog"].as_array().into_iter().flatten() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let bytes = STANDARD
            .decode(string(item, "data_b64"))
            .map_err(|e| e.to_string())?;
        let mut signal: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if super::identity::canonical_node_id(string(&signal, "to"))
            .ok()
            .as_deref()
            != Some(id)
        {
            continue;
        }
        signal["to"] = json!(id);
        let admission_hub_now =
            hub_now.saturating_add(observed.elapsed().as_millis().min(u64::MAX as u128) as u64);
        let created = item["created_ms"].as_u64().unwrap_or(admission_hub_now);
        let acked = item
            .get("acked_at_ms")
            .filter(|v| v.is_number())
            .or_else(|| signal.get("acked_at_ms"))
            .unwrap_or(&Value::Null);
        let admitted = local.admit(
            hub,
            row,
            &signal,
            item["seq"].as_u64().unwrap_or(0),
            &iso(created),
            LocalState::signal_deadline(created, admission_hub_now, now_ms()),
            acked,
        )?;
        if admitted && !acked.is_number() {
            on_admit(&signal);
        }
        client.rpc(
            "consumer_ack",
            &json!({"stream":"CITO_SIGNALS","name":name,"seq":item["seq"]}),
        )?;
    }
    drop(registration_guard);
    // Business acknowledgments can arrive after transport cursor advancement. Reconcile
    // unread rows from durable stream metadata even when no new backlog item is emitted.
    let cursor = local
        .listener(hub, id)?
        .map(|r| r["ack_scan_seq"].as_u64().unwrap_or(0))
        .unwrap_or(0);
    let mut current = local.current_signals(hub, id, usize::MAX)?;
    current.sort_by_key(|signal| signal["seq"].as_u64().unwrap_or(0));
    let mut pending: Vec<_> = current
        .iter()
        .filter(|signal| signal["seq"].as_u64().unwrap_or(0) > cursor)
        .take(128)
        .cloned()
        .collect();
    if pending.is_empty() && cursor > 0 {
        pending = current.into_iter().take(128).collect();
    }
    if !pending.is_empty() {
        if let Ok(remote) = client.rpc("stream_get_message",&json!({"stream":"CITO_SIGNALS","seqs":pending.iter().map(|r|r["seq"].clone()).collect::<Vec<_>>()})) {
            local.db.transaction(|db| {
                for message in remote["messages"].as_array().into_iter().flatten() {
                    if !message["acked_at_ms"].is_null() && pending.iter().any(|p|p["seq"] == message["seq"] && p["signal_id"] == message["signal_id"]) {
                        db.execute("UPDATE local_signals SET acknowledged_at_ms=COALESCE(acknowledged_at_ms,?),state=CASE WHEN state='RECEIVED' THEN 'STORED_PASSIVE' ELSE state END WHERE hub=? AND listener_id=? AND signal_id=?",&[message["acked_at_ms"].clone(),json!(hub),json!(id),message["signal_id"].clone()])?;
                        db.execute("DELETE FROM wake_retries WHERE hub=? AND listener_id=?", &[json!(hub),json!(id)])?;
                    }
                }
                db.execute("UPDATE local_listeners SET ack_scan_seq=? WHERE hub=? AND listener_id=?",&[pending.last().map(|r|r["seq"].clone()).unwrap_or(json!(0)),json!(hub),json!(id)])?;
                Ok(())
            })?;
        }
    }
    let now = now_ms();
    let retries = local.db.query(
        "SELECT retry_at_ms FROM wake_retries WHERE hub=? AND listener_id=?",
        &[json!(hub), json!(id)],
    )?;
    if retries
        .first()
        .and_then(|v| v["retry_at_ms"].as_u64())
        .unwrap_or(0)
        > now
    {
        return Ok(());
    }
    // A losing channel retries after the remote claim lease, preserving unread state.
    local.restore_passive(hub, row)?;
    if string(row, "harness") == "human" {
        return Ok(());
    }
    let Some(plugin) = plugin else {
        return Ok(());
    };
    dispatch_listener(
        local,
        client,
        row,
        plugin,
        state_dir,
        Duration::from_secs(10),
    )
}

pub fn probe_listener(
    local: &LocalState,
    client: &HubClient,
    row: &Value,
    plugin: &PluginDescriptor,
    state_dir: &std::path::Path,
) -> Result<(), String> {
    let result = plugin_request(
        plugin,
        &json!({"op":"probe","session_id":row["session_id"],"endpoint_pid":row["endpoint_pid"],"cwd":row["cwd"],"state_dir":state_dir}),
        None,
        Some((local, row)),
    ).unwrap_or_else(|error| json!({"ok":false,"error":error}));
    apply_probe(local, client, row, &result)
}

pub(super) fn apply_probe(
    local: &LocalState,
    client: &HubClient,
    row: &Value,
    result: &Value,
) -> Result<(), String> {
    let status = if result["ok"] == true {
        string(result, "status")
    } else {
        "unknown"
    };
    let failures = if status == "alive" {
        0
    } else {
        row["consecutive_probe_failures"].as_u64().unwrap_or(0) + 1
    };
    let revoke = matches!(
        status,
        "switched" | "closed_definitive" | "session_switched"
    ) || failures >= 3;
    // Never hold the local database mutex while waiting for registration authority.
    let registration = local.registration_lock(&client.name)?;
    let _registration = if revoke {
        Some(
            registration
                .lock()
                .map_err(|_| "STATE_ERROR: Registration lock poisoned")?,
        )
    } else {
        None
    };
    let revoked = local.db.transaction(|c| {
        c.execute("UPDATE local_listeners SET consecutive_probe_failures=CASE WHEN ?=0 THEN 0 ELSE consecutive_probe_failures+1 END WHERE hub=? AND listener_id=? AND binding_generation=? AND owner_token=? AND status='ACTIVE'", &[json!(failures),row["hub"].clone(),row["listener_id"].clone(),row["binding_generation"].clone(),row["owner_token"].clone()])?;
        if revoke {
            let changed = c.execute("UPDATE local_listeners SET status='UNLISTENED',retirement_reason='hard_exit',updated_at=? WHERE hub=? AND listener_id=? AND binding_generation=? AND owner_token=? AND status='ACTIVE'", &[json!(now_ms()),row["hub"].clone(),row["listener_id"].clone(),row["binding_generation"].clone(),row["owner_token"].clone()])?;
            if changed > 0 {
                c.execute("DELETE FROM wake_retries WHERE hub=? AND listener_id=?", &[row["hub"].clone(),row["listener_id"].clone()])?;
                c.execute("UPDATE local_signals SET state='STORED_PASSIVE',held_reason='unlistened',dispatcher_token=NULL WHERE hub=? AND listener_id=? AND state IN ('RECEIVED','DISPATCHING') AND acknowledged_at_ms IS NULL", &[row["hub"].clone(),row["listener_id"].clone()])?;
                c.execute("INSERT INTO pending_hub_detaches(hub,listener_id,hub_url) VALUES(?,?,?) ON CONFLICT(hub,listener_id) DO UPDATE SET hub_url=excluded.hub_url", &[row["hub"].clone(),row["listener_id"].clone(),client.config["url"].clone()])?;
            }
            return Ok(changed > 0);
        }
        Ok(false)
    })?;
    if revoked {
        local.invalidate_discovery(string(row, "harness"))?;
        let channel = format!("{}:{}", string(row, "host_id"), string(row, "harness"));
        if detach_channel_expected(client, string(row, "listener_id"), &channel, row).is_ok() {
            local.clear_pending_detach_if_current(
                string(row, "hub"),
                string(row, "listener_id"),
                row["binding_generation"].as_u64().unwrap_or(0),
                string(row, "owner_token"),
            )?;
        }
    }
    Ok(())
}

fn dispatch_listener(
    local: &LocalState,
    client: &HubClient,
    row: &Value,
    plugin: &PluginDescriptor,
    state_dir: &std::path::Path,
    renewal_interval: Duration,
) -> Result<(), String> {
    let id = string(row, "listener_id");
    let hub = &client.name;
    let channel = format!("{}:{}", string(row, "host_id"), string(row, "harness"));
    let batch = local.claim_wake_batch_for(hub, row, qualify_reply_ids(state_dir, hub))?;
    if batch.is_empty() {
        return Ok(());
    }
    let first = &batch[0];
    let ids: Vec<Value> = batch.iter().map(|r| r["signal_id"].clone()).collect();
    let claim = json!({"node_id":id,"signal_ids":ids,"channel_id":channel,"lease_ms":30_000});
    // This RPC is unconditional, even with one channel. Transport errors fail closed.
    let claimed = match client.rpc("signal_claim_dispatch", &claim) {
        Ok(v) => v,
        Err(e) => {
            local.finish_wake_batch(row, first, "RECEIVED", None, false, false, false, false)?;
            local.schedule_wake_retry_for(row, 1500)?;
            return Err(e);
        }
    };
    if claimed["claimed"] != true {
        if let Some(acked) = claimed["acked_ids"].as_array() {
            local.acknowledge(hub, id, acked, &channel)?;
        }
        let re_batch = claimed["reason"] == "already_acked";
        local.finish_wake_batch(
            row,
            first,
            if re_batch {
                "RECEIVED"
            } else {
                "STORED_PASSIVE"
            },
            None,
            false,
            false,
            false,
            re_batch,
        )?;
        local.schedule_wake_retry_for(row, if re_batch { 0 } else { 30_500 })?;
        return Ok(());
    }
    let queued = batch
        .iter()
        .all(|r| r["queue_written"].as_i64().unwrap_or(0) != 0);
    let confirmed = batch
        .iter()
        .all(|r| r["queue_confirmed"].as_i64().unwrap_or(0) != 0);
    let payloads: Vec<_> = batch
        .iter()
        .map(|row| {
            let mut row = row.clone();
            if let Some(row) = row.as_object_mut() {
                for key in [
                    "state",
                    "dispatcher_token",
                    "batch_card",
                    "batch_expires_at_ms",
                    "acknowledged_at_ms",
                    "queue_written",
                    "queue_confirmed",
                ] {
                    row.remove(key);
                }
            }
            row
        })
        .collect();
    let request = json!({"op":"wake","session_id":row["session_id"],"cwd":row["cwd"],"endpoint_pid":row["endpoint_pid"],"batch_id":first["batch_id"],"wake":true,"checkpoint_only":true,"text":first["batch_card"],"signals":payloads,"queue_written":queued,"queue_confirmed":confirmed,"state_dir":state_dir,"env":{}});
    let mut fenced = row.clone();
    fenced["wake_batch"] = first.clone();
    match plugin_request_owned(
        plugin,
        &request,
        Some((client, &claim)),
        Some((local, &fenced)),
        renewal_interval,
    ) {
        Ok(result) if result["ok"] == true => {
            // One request produces one final response. These checkpoints cannot
            // cover a process death between queue insertion and that response.
            // Multi-effect plugins return continue_wake after insertion. The next
            // request repeats ownership and unreadness checks before a later effect.
            // Completion of a single submitted effect may race a business ack.
            let written = result["queue_written"] == true;
            let insertion_confirmed = written
                && result["insertion_unknown"] != true
                && result["queue_confirmed"].as_bool().unwrap_or(!queued);
            if result["continue_wake"] == true && written {
                local.finish_wake_batch(
                    row,
                    first,
                    "DISPATCHING",
                    None,
                    written,
                    insertion_confirmed,
                    false,
                    false,
                )?;
                local.schedule_wake_retry_for(row, 0)?;
                return Ok(());
            }
            match string(&result, "outcome") {
                "delivered" | "delivered_unconfirmed" => {
                    // A retry of a confirmed queue insertion can acknowledge only
                    // when the independent open effect also reports confirmation.
                    let delivered = result["outcome"] == "delivered"
                        || ((confirmed || insertion_confirmed) && result["open_confirmed"] == true);
                    if local.finish_wake_batch(
                        row,
                        first,
                        "DISPATCHED",
                        None,
                        written,
                        insertion_confirmed,
                        delivered,
                        false,
                    )? && delivered
                    {
                        // The same transaction as completion persists pending_hub_acks.
                        // Leave them for the daemon's durable flush if the Hub is offline.
                        let _ = client.rpc(
                            "signal_ack",
                            &json!({"node_id":id,"signal_ids":ids,"channel_id":channel}),
                        );
                    }
                }
                "session_switched" => {
                    // Serialize terminal revocation and detach with registration.
                    let registration = local.registration_lock(&client.name)?;
                    let _registration = registration
                        .lock()
                        .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
                    local.finish_wake_batch(
                        row,
                        first,
                        "STORED_PASSIVE",
                        Some("session_switched"),
                        written,
                        insertion_confirmed,
                        false,
                        false,
                    )?;
                    let changed=local.db.transaction(|c| {
                        let changed=c.execute("UPDATE local_listeners SET status='UNLISTENED',retirement_reason='hard_exit',updated_at=? WHERE hub=? AND listener_id=? AND status='ACTIVE' AND binding_generation=? AND COALESCE(owner_token,'')=?", &[json!(now_ms()),json!(hub), json!(id), row["binding_generation"].clone(), row["owner_token"].clone()])?;
                        if changed>0 {
                            c.execute("INSERT INTO pending_hub_detaches(hub,listener_id,hub_url) VALUES(?,?,?) ON CONFLICT(hub,listener_id) DO UPDATE SET hub_url=excluded.hub_url", &[json!(hub),json!(id),client.config["url"].clone()])?;
                        }
                        Ok(changed)
                    })?;
                    if changed > 0 {
                        local.invalidate_discovery(string(row, "harness"))?;
                        detach_channel_expected(client, id, &channel, row)?;
                        local.clear_pending_detach_if_current(
                            hub,
                            id,
                            row["binding_generation"].as_u64().unwrap_or(0),
                            string(row, "owner_token"),
                        )?;
                    }
                }
                "driver_unavailable" if written => {
                    // The final response confirms insertion but could not finish
                    // the remaining wake effect. Preserve its checkpoint for retry.
                    local.finish_wake_batch(
                        row,
                        first,
                        "DISPATCHING",
                        None,
                        written,
                        insertion_confirmed,
                        false,
                        false,
                    )?;
                    local.schedule_wake_retry_for(row, 1500)?;
                }
                "driver_unavailable" => {
                    local.finish_wake_batch(
                        row,
                        first,
                        "STORED_PASSIVE",
                        Some("driver_unavailable"),
                        written,
                        insertion_confirmed,
                        false,
                        false,
                    )?;
                    local.schedule_wake_retry_for(row, 1500)?;
                }
                _ => {
                    local.finish_wake_batch(
                        row,
                        first,
                        "DISPATCHING",
                        None,
                        written,
                        insertion_confirmed,
                        false,
                        false,
                    )?;
                    local.schedule_wake_retry_for(row, 1500)?;
                }
            }
        }
        Err(e) => {
            let current = local.listener(hub, id)?;
            let inactive = current.as_ref().is_none_or(|r| r["status"] != "ACTIVE");
            let reason = if current
                .as_ref()
                .is_some_and(|r| r["status"] == "UNLISTENED")
            {
                Some("unlistened_before_dispatch")
            } else if inactive {
                Some("fenced_before_dispatch")
            } else {
                None
            };
            let re_batch = e.claim_loss == Some(ClaimLoss::AlreadyAcked);
            let lost_claim = e.claim_loss == Some(ClaimLoss::AlreadyClaimed);
            let next = if inactive || lost_claim {
                "STORED_PASSIVE"
            } else if re_batch {
                "RECEIVED"
            } else {
                "DISPATCHING"
            };
            local.finish_wake_batch(row, first, next, reason, false, false, false, re_batch)?;
            local.schedule_wake_retry_for(
                row,
                if lost_claim {
                    30_500
                } else if re_batch {
                    0
                } else {
                    1500
                },
            )?;
            return Err(e.message);
        }
        Ok(result) => {
            local.finish_wake_batch(row, first, "DISPATCHING", None, false, false, false, false)?;
            local.schedule_wake_retry_for(row, 1500)?;
            return Err(crate::support::response_diagnostic(
                &result,
                "Session delivery failed",
            ));
        }
    }
    Ok(())
}
pub fn detach_channel_expected(
    client: &HubClient,
    id: &str,
    channel: &str,
    captured: &Value,
) -> Result<(), String> {
    if channel.ends_with(":human") {
        client.human_request("human_device_detach", &json!({"node_id":id,"channel_id":channel,"binding_generation":captured["binding_generation"],"owner_token":captured["owner_token"]}))?;
        return Ok(());
    }
    for _ in 0..3 {
        let Some((mut remote, revision)) = client.kv_get(id)? else {
            return Ok(());
        };
        if !remote.is_object() {
            return Err("INVALID_INPUT: Listener must be an object".into());
        }
        if !remote_matches_listener(&remote, captured, channel) {
            return Err("STALE_GENERATION: Listener changed during channel detach".into());
        }
        if let Some(ch) = remote["channels"]
            .as_object_mut()
            .and_then(|c| c.get_mut(channel))
        {
            ch.as_object_mut()
                .ok_or("INVALID_INPUT: Listener channel must be an object")?
                .insert("state".into(), json!("UNLISTENED"));
        }
        super::lease::reconcile_registration_state(&mut remote, client.server_now());
        match client.kv_save(id, &remote, Some(revision)) {
            Ok(_) => return Ok(()),
            Err(error) if error.starts_with("10071:") => {}
            Err(error) => return Err(error),
        }
    }
    Err("STALE_GENERATION: Channel detach CAS retries exhausted".into())
}

fn qualify_reply_ids(state_dir: &std::path::Path, hub: &str) -> bool {
    let path = std::env::var("CITO_CONFIG_PATH")
        .ok()
        .and_then(|raw| {
            crate::plugins::expand_home_dir(
                &raw,
                "CITO_CONFIG_PATH",
                std::env::var("HOME").ok().as_deref(),
            )
            .ok()
        })
        .unwrap_or_else(|| state_dir.join("config.json"));
    let config = std::fs::read_to_string(path).ok();
    let Some((configured_default, hubs, _)) = config
        .as_deref()
        .and_then(|raw| super::config::try_normalize_existing_config(Some(raw)).ok())
    else {
        return !matches!(hub, "local" | "default");
    };
    let enabled = hubs.values().filter(|h| h["enabled"] != false).count();
    let count = if enabled == 0 { hubs.len() } else { enabled };
    let default = std::env::var("CITO_HUB")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let default = default.as_deref().unwrap_or(&configured_default);
    count != 1 || !matches!(default, "local" | "default")
}

#[cfg(test)]
#[path = "receiver_tests.rs"]
mod tests;
