use serde_json::{json, Value};

/// Reconcile channel expiry and explicit personal pause before publishing a registration.
pub fn reconcile_registration_state(reg: &mut Value, now: u64) {
    let paused = reg["human_paused"].as_bool().unwrap_or(false);
    let mut active = false;
    if let Some(channels) = reg["channels"].as_object_mut() {
        for channel in channels.values_mut() {
            if channel["host_id"] != "hub"
                && now.saturating_sub(channel["updated_at_ms"].as_u64().unwrap_or(0)) > 1_800_000
            {
                channel["state"] = json!("UNLISTENED");
            }
            active |= channel["state"] == "ACTIVE";
        }
    }
    reg["state"] = json!(if !paused && active {
        "ACTIVE"
    } else {
        "UNLISTENED"
    });
}

/// A live Hub-hosted attachment is permanent; host channels retain a bounded reconnect window.
pub fn evaluate_node_lease(registration: &Value, now: u64) -> &'static str {
    if registration["human_paused"] == true || registration["state"] != "ACTIVE" {
        return "UNLISTENED";
    }
    let channels = registration["channels"].as_object();
    if channels.is_none_or(|c| c.is_empty()) {
        let age = now.saturating_sub(registration["updated_at_ms"].as_u64().unwrap_or(now));
        return if age < 60_000 {
            "ACTIVE"
        } else if age <= 1_800_000 {
            "DISCONNECTED_RETAINED"
        } else {
            "EXPIRED"
        };
    }
    let mut retained = false;
    for channel in channels
        .into_iter()
        .flat_map(|c| c.values())
        .filter(|c| c["state"] == "ACTIVE")
    {
        if channel["host_id"] == "hub" {
            return "ACTIVE";
        }
        let age = now.saturating_sub(channel["updated_at_ms"].as_u64().unwrap_or(now));
        if age < 60_000 {
            return "ACTIVE";
        }
        if age <= 1_800_000 {
            retained = true;
        }
    }
    if retained {
        "DISCONNECTED_RETAINED"
    } else {
        "EXPIRED"
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn exact_lease_boundaries_for_legacy_and_attached_nodes() {
        for row in [
            json!({"state":"ACTIVE","updated_at_ms":1000}),
            json!({"state":"ACTIVE","updated_at_ms":1000,"channels":{}}),
            json!({"state":"ACTIVE","channels":{"local":{"state":"ACTIVE","updated_at_ms":1000}}}),
        ] {
            assert_eq!(evaluate_node_lease(&row, 60_999), "ACTIVE");
            assert_eq!(evaluate_node_lease(&row, 61_000), "DISCONNECTED_RETAINED");
            assert_eq!(
                evaluate_node_lease(&row, 1_801_000),
                "DISCONNECTED_RETAINED"
            );
            assert_eq!(evaluate_node_lease(&row, 1_801_001), "EXPIRED");
        }
    }
    #[test]
    fn permanent_hub_channel_and_host_ttl() {
        let mut row = json!({"state":"ACTIVE","channels":{"a":{"state":"ACTIVE","host_id":"hub","updated_at_ms":1},"b":{"state":"ACTIVE","host_id":"machine","updated_at_ms":1}}});
        assert_eq!(evaluate_node_lease(&row, 4_000_000), "ACTIVE");
        row["channels"]["a"]["state"] = json!("UNLISTENED");
        assert_eq!(evaluate_node_lease(&row, 4_000_000), "EXPIRED");
        assert_eq!(evaluate_node_lease(&row, 100_000), "DISCONNECTED_RETAINED");
        assert_eq!(evaluate_node_lease(&row, 10_000), "ACTIVE");
        row["channels"]["a"]["state"] = json!("ACTIVE");
        row["human_paused"] = json!(true);
        reconcile_registration_state(&mut row, 1_800_001);
        assert_eq!(row["state"], "UNLISTENED");
        assert_eq!(row["channels"]["b"]["state"], "ACTIVE");
        assert_eq!(evaluate_node_lease(&row, 1_800_001), "UNLISTENED");
        reconcile_registration_state(&mut row, 1_800_002);
        assert_eq!(row["channels"]["b"]["state"], "UNLISTENED");
        row["human_paused"] = json!(false);
        reconcile_registration_state(&mut row, 4_000_000);
        assert_eq!(row["state"], "ACTIVE");
        row["channels"]["a"]["state"] = json!("UNLISTENED");
        reconcile_registration_state(&mut row, 4_000_000);
        assert_eq!(row["state"], "UNLISTENED");
    }
}
