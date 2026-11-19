//! Activity persistence transitions; HTTP/UI callers only select operations and project results.
use super::*;
impl LocalState {
    pub(in crate::core) fn acknowledge_activity(
        &self,
        action: &Value,
        default_filter: &str,
        host_id: &str,
        bulk: bool,
    ) -> Result<(), String> {
        let ids = action["signal_ids"].as_array().cloned().unwrap_or_default();
        let rows = self.db.query(
            "SELECT s.*,l.hub_url AS target_hub_url,l.principal_id AS target_principal_id,l.status AS target_status,l.harness AS target_harness,l.owner_user AS target_user,l.title AS target_title FROM local_signals s LEFT JOIN local_listeners l ON l.hub=s.hub AND l.listener_id=s.listener_id WHERE s.activity_hidden=0 AND s.acknowledged_at_ms IS NULL AND (?!='me' OR l.harness='human')",
            &[json!(if bulk { action["filter"].as_str().unwrap_or(default_filter) } else { "all" })],
        )?;
        for row in rows {
            if !crate::core::state::joined_signal_is_current(&row) {
                continue;
            }
            let requested_hub = string(action, "hub");
            if !requested_hub.is_empty() && row["hub"] != requested_hub {
                continue;
            }
            if bulk
                && !crate::core::state::matches_activity(
                    &row,
                    action["query"].as_str().unwrap_or(""),
                )
            {
                continue;
            }
            if bulk
                || ids.iter().any(|id| {
                    id == &row["signal_id"]
                        || id.as_str()
                            == Some(
                                format!("{}:{}", string(&row, "hub"), string(&row, "signal_id"))
                                    .as_str(),
                            )
                        || id == &row["batch_id"]
                })
            {
                self.acknowledge(
                    string(&row, "hub"),
                    string(&row, "listener_id"),
                    &[row["signal_id"].clone()],
                    &format!("{}:{}", host_id, string(&row, "target_harness")),
                )?;
            }
        }
        Ok(())
    }
    pub(in crate::core) fn activity_message(
        &self,
        hub: &str,
        signal_id: &Value,
    ) -> Result<Value, String> {
        let row = self.db
            .query(
                "SELECT s.*,l.hub_url AS target_hub_url,l.principal_id AS target_principal_id,l.status AS target_status FROM local_signals s LEFT JOIN local_listeners l ON s.hub=l.hub AND s.listener_id=l.listener_id WHERE s.hub=? AND s.signal_id=?",
                &[json!(hub), signal_id.clone()],
            )?
            .into_iter()
            .next()
            .filter(crate::core::state::joined_signal_is_current)
            .ok_or("LISTENER_NOT_FOUND: Message no longer available")?;
        Ok(row)
    }
    pub(in crate::core) fn clear_activity_history(
        &self,
        action: &Value,
        default_filter: &str,
        host_id: &str,
    ) -> Result<(), String> {
        self.db.transaction(|c| {
            let selected = action["hub"].as_str().unwrap_or("");
            let signal_id = action["signal_id"].as_str().unwrap_or("");
            let filter=action["filter"].as_str().unwrap_or(default_filter);
            c.query_while("SELECT s.*,l.hub_url AS target_hub_url,l.principal_id AS target_principal_id,l.status AS target_status,l.harness AS target_harness,l.owner_user AS target_user,l.title AS target_title FROM local_signals s LEFT JOIN local_listeners l ON l.hub=s.hub AND l.listener_id=s.listener_id WHERE s.activity_hidden=0 AND (?='' OR s.hub=?) AND (?='' OR signal_id=?) AND (?!='me' OR l.harness='human') AND (?!='unread' OR s.acknowledged_at_ms IS NULL)", &[json!(selected),json!(selected),json!(signal_id),json!(signal_id),json!(filter),json!(filter)],|row| {
                if !crate::core::state::joined_signal_is_current(&row) || !crate::core::state::matches_activity(&row,action["query"].as_str().unwrap_or("")) { return Ok(true); }
                if row["target_harness"] == "human" && row["acknowledged_at_ms"].is_null() { c.execute("INSERT OR REPLACE INTO pending_hub_acks(hub,node_id,signal_id,channel_id,acked_at_ms,hub_url,principal_id) VALUES(?,?,?,?,?,?,?)", &[row["hub"].clone(),row["listener_id"].clone(),row["signal_id"].clone(),json!(format!("{}:human",host_id)),json!(now_ms()),row["hub_url"].clone(),row["principal_id"].clone()])?; }
                c.execute("UPDATE local_signals SET activity_hidden=1,acknowledged_at_ms=CASE WHEN ?='human' THEN COALESCE(acknowledged_at_ms,?) ELSE acknowledged_at_ms END WHERE hub=? AND signal_id=?", &[row["target_harness"].clone(),json!(now_ms()),row["hub"].clone(),row["signal_id"].clone()])?;
                crate::core::state::reconcile_wake_retry(c, string(&row, "hub"), string(&row, "listener_id"))?;
                Ok(true)
            })?;
            Ok(())
        })?;
        Ok(())
    }
}
