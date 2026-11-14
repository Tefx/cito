use serde_json::{json, Value};

/// Device timestamps are Hub timestamps; explicit detach remains independent of expiry.
pub fn attachment_availability(channel: &Value, hub_now_ms: u64) -> &'static str {
    if channel["state"] != "ACTIVE" {
        return "UNLISTENED";
    }
    if channel["host_id"] == "hub" {
        return "ACTIVE";
    }
    match hub_now_ms.saturating_sub(channel["updated_at_ms"].as_u64().unwrap_or(0)) {
        0..60_000 => "ACTIVE",
        60_000..=1_800_000 => "DISCONNECTED_RETAINED",
        _ => "EXPIRED",
    }
}

/// Cached projections use the server clock recorded by their Hub snapshot.
pub fn registration_now(registration: &Value, fallback: u64) -> u64 {
    registration["server_now_ms"].as_u64().unwrap_or(fallback)
}

/// Pause affects Node availability, never device ownership or the retained profile.
pub fn evaluate_node_lease(registration: &Value, now: u64) -> &'static str {
    if registration["human_paused"] == true {
        return "UNLISTENED";
    }
    let now = registration_now(registration, now);
    let channels = registration["channels"].as_object();
    if channels.is_none_or(|c| c.is_empty()) {
        if registration["state"] != "ACTIVE" {
            return "UNLISTENED";
        }
        let channel = json!({"state":"ACTIVE","updated_at_ms":registration["updated_at_ms"].as_u64().unwrap_or(now)});
        return attachment_availability(&channel, now);
    }
    let mut retained = false;
    let mut expired = false;
    for channel in channels.into_iter().flat_map(|c| c.values()) {
        match attachment_availability(channel, now) {
            "ACTIVE" => return "ACTIVE",
            "DISCONNECTED_RETAINED" => retained = true,
            "EXPIRED" => expired = true,
            _ => {}
        }
    }
    if retained {
        "DISCONNECTED_RETAINED"
    } else if expired {
        "EXPIRED"
    } else {
        "UNLISTENED"
    }
}

pub fn reconcile_registration_state(reg: &mut Value, now: u64) {
    // Reconciliation at the Hub always uses its current clock, not a cached stamp.
    if let Some(fields) = reg.as_object_mut() {
        fields.remove("server_now_ms");
    }
    // Keep generation high-water marks after pruning so delayed attach retries
    // cannot resurrect an expired or superseded device incarnation.
    let channels = reg["channels"].as_object().cloned().unwrap_or_default();
    for (id, channel) in &channels {
        if !id.ends_with(":human") || channel["host_id"] == "hub" {
            continue;
        }
        let availability = attachment_availability(channel, now);
        let superseded = availability == "UNLISTENED"
            && channel["host_name"]
                .as_str()
                .is_some_and(|name| !name.trim().is_empty())
            && channels.iter().any(|(other_id, other)| {
                other_id != id
                    && other["host_name"] == channel["host_name"]
                    && matches!(
                        attachment_availability(other, now),
                        "ACTIVE" | "DISCONNECTED_RETAINED"
                    )
            });
        if availability == "EXPIRED" || superseded {
            if !reg["device_generation_fences"].is_object() {
                reg["device_generation_fences"] = json!({});
            }
            let previous = reg["device_generation_fences"][id].as_u64().unwrap_or(0);
            reg["device_generation_fences"][id] =
                json!(previous.max(channel["binding_generation"].as_u64().unwrap_or(0)));
            if let Some(entries) = reg["channels"].as_object_mut() {
                entries.remove(id);
            }
        }
    }
    reg["state"] = json!(if matches!(
        evaluate_node_lease(reg, now),
        "ACTIVE" | "DISCONNECTED_RETAINED"
    ) {
        "ACTIVE"
    } else {
        "UNLISTENED"
    });
}

/// Public attachment projection deliberately excludes fencing tokens and credentials.
pub fn attachments(reg: &Value, local_host: &str, now: u64) -> Vec<Value> {
    let now = registration_now(reg, now);
    let mut projected = std::collections::BTreeMap::<(String, String), Value>::new();
    for (id, channel) in reg["channels"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.iter())
    {
        let availability = attachment_availability(channel, now);
        if !matches!(availability, "ACTIVE" | "DISCONNECTED_RETAINED") {
            continue;
        }
        let hub = channel["host_id"] == "hub";
        let host = channel["host_id"].as_str().unwrap_or("");
        let display = channel["display_name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| channel["host_name"].as_str().filter(|s| !s.is_empty()))
            .unwrap_or(if hub { "Channel" } else { "Device" });
        let mut attachment = json!({"channel_id":id,"location":if hub{"hub"}else{"device"},"display_name":display,"availability":attachment_availability(channel,now),"is_local":!hub && host==local_host});
        if hub {
            attachment["plugin_id"] = json!(id.strip_prefix("hub:").unwrap_or(id));
        } else {
            attachment["host_id"] = json!(host);
        }
        let key = (
            if hub { "hub" } else { "device" }.to_owned(),
            display.to_owned(),
        );
        let rank = |a: &Value| (a["is_local"] == true, a["availability"] == "ACTIVE");
        if projected
            .get(&key)
            .is_none_or(|old| rank(&attachment) > rank(old))
        {
            projected.insert(key, attachment);
        }
    }
    projected.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_deduplicates_live_attachments_and_pruning_retains_fences() {
        let mut reg = json!({"harness":"human","human_paused":false,"scope":"self","human_control_revision":4,"channels":{
            "old:human":{"host_id":"old","host_name":"Laptop","state":"UNLISTENED","binding_generation":7},
            "unknown:human":{"host_id":"unknown","state":"UNLISTENED","binding_generation":2},
            "remote:human":{"host_id":"remote","host_name":"Laptop","state":"ACTIVE","updated_at_ms":1,"binding_generation":1},
            "local:human":{"host_id":"local","host_name":"Laptop","state":"ACTIVE","updated_at_ms":100000,"binding_generation":2},
            "expired:human":{"host_id":"expired","host_name":"Retired","state":"ACTIVE","updated_at_ms":1,"binding_generation":9},
            "hub:messenger":{"host_id":"hub","display_name":"Messenger","state":"ACTIVE"}
        }});
        let projected = attachments(&reg, "local", 100001);
        assert_eq!(projected.len(), 3);
        let laptop = projected
            .iter()
            .find(|a| a["display_name"] == "Laptop")
            .unwrap();
        assert_eq!(laptop["host_id"], "local");
        assert_eq!(laptop["availability"], "ACTIVE");
        reg["channels"]["expired:human"]["updated_at_ms"] = json!(0);
        reg["channels"]["remote:human"]["updated_at_ms"] = json!(1900000);
        reg["channels"]["local:human"]["updated_at_ms"] = json!(2000000);
        reconcile_registration_state(&mut reg, 2000001);
        assert!(reg["channels"].get("old:human").is_none());
        assert!(reg["channels"].get("expired:human").is_none());
        assert!(
            reg["channels"].get("unknown:human").is_some(),
            "Missing hostnames never establish supersession"
        );
        assert_eq!(reg["device_generation_fences"]["old:human"], 7);
        assert_eq!(reg["device_generation_fences"]["expired:human"], 9);
        assert_eq!(reg["human_control_revision"], 4);
        assert_eq!(reg["scope"], "self");
        assert_eq!(attachments(&reg, "local", 2000001).len(), 2);
    }
    #[test]
    fn exact_hub_clock_boundaries_and_stale_summary() {
        let mut reg = json!({"harness":"human","human_paused":false,"about":"Keep me","scope":"self","human_control_revision":3,"state":"UNLISTENED","channels":{"a":{"host_id":"laptop","state":"ACTIVE","updated_at_ms":1000,"owner_token":"private"}}});
        for (now, expected) in [
            (60_999, "ACTIVE"),
            (61_000, "DISCONNECTED_RETAINED"),
            (1_801_000, "DISCONNECTED_RETAINED"),
            (1_801_001, "EXPIRED"),
        ] {
            assert_eq!(evaluate_node_lease(&reg, now), expected);
            reconcile_registration_state(&mut reg, now);
            assert_eq!(reg["channels"]["a"]["state"], "ACTIVE");
            assert_eq!(reg["about"], "Keep me");
            assert_eq!(reg["human_control_revision"], 3);
        }
        reg["server_now_ms"] = json!(1001);
        assert_eq!(evaluate_node_lease(&reg, u64::MAX), "ACTIVE");
        let projected = attachments(&reg, "laptop", u64::MAX);
        assert_eq!(projected[0]["is_local"], true);
        assert!(projected[0].get("owner_token").is_none());
        reg["channels"]["hub:messenger"] =
            json!({"host_id":"hub","state":"ACTIVE","updated_at_ms":1});
        reconcile_registration_state(&mut reg, 4_000_000);
        assert_eq!(reg["state"], "ACTIVE");
        reg["human_paused"] = json!(true);
        reconcile_registration_state(&mut reg, 4_000_000);
        assert_eq!(reg["state"], "UNLISTENED");
        assert_eq!(reg["channels"]["hub:messenger"]["state"], "ACTIVE");
    }
}
