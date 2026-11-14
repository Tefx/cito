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
    reg["channels"].as_object().into_iter().flat_map(|m| m.iter()).map(|(id, channel)| {
        let hub = channel["host_id"] == "hub";
        let host = channel["host_id"].as_str().unwrap_or("");
        let display = channel["display_name"].as_str().filter(|s| !s.is_empty())
            .or_else(|| channel["host_name"].as_str().filter(|s| !s.is_empty()))
            .unwrap_or(if hub { "Channel" } else { "Device" });
        let mut attachment = json!({"channel_id":id,"location":if hub{"hub"}else{"device"},"display_name":display,"availability":attachment_availability(channel,now),"is_local":!hub && host==local_host});
        if hub { attachment["plugin_id"] = json!(id.strip_prefix("hub:").unwrap_or(id)); }
        else { attachment["host_id"] = json!(host); }
        attachment
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
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
