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
        let ids = action["signal_ids"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let hub = string(action, "hub");
        let (filter, query) = if bulk {
            (
                action["filter"].as_str().unwrap_or(default_filter),
                string(action, "query").trim(),
            )
        } else {
            ("all", "")
        };
        // Only a text search needs the payload. Stream those rows through the same Unicode
        // and address matcher as the activity view rather than collecting search results.
        let search = if query.is_empty() {
            ""
        } else {
            ",s.message,s.from_user,s.from_title,s.from_host_name,s.from_listener_id,s.from_harness,l.owner_user AS target_user,l.title AS target_title"
        };
        self.db.transaction(|c| {
            let mut groups = std::collections::HashMap::<_,Vec<Value>>::new();
            c.query_each(&format!("SELECT s.hub,s.listener_id,s.signal_id,s.batch_id,s.hub_url,s.principal_id,l.hub_url AS target_hub_url,l.principal_id AS target_principal_id,l.status AS target_status,l.harness AS target_harness{search} FROM local_signals s JOIN local_listeners l ON l.hub=s.hub AND l.listener_id=s.listener_id WHERE s.activity_hidden=0 AND s.acknowledged_at_ms IS NULL AND (?1='' OR s.hub=?1) AND (?2!='me' OR l.harness='human')"), &[json!(hub),json!(filter)], |row| {
                let hub = string(&row,"hub");
                let id = string(&row,"signal_id");
                let selected = bulk || ids.iter().filter_map(Value::as_str).any(|key| key == id || Some(key) == row["batch_id"].as_str() || key.strip_prefix(hub).and_then(|s|s.strip_prefix(':')) == Some(id));
                if selected && joined_signal_is_current(&row) && (query.is_empty() || matches_activity(&row,query)) {
                    let node = string(&row,"listener_id");
                    groups.entry((hub.to_owned(),node.to_owned(),format!("{host_id}:{}",string(&row,"target_harness")))).or_default().push(row["signal_id"].clone());
                }
            })?;
            // Finish stepping the selection before modifying any indexed unread state.
            for ((hub,node,channel),selected) in groups {
                for id in selected { acknowledge_current(c,&hub,&node,&id,&channel)?; }
                reconcile_wake_retry(c,&hub,&node)?;
            }
            Ok(())
        })
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
                if row["target_harness"] == "human" && row["acknowledged_at_ms"].is_null() { acknowledge_current(c,string(&row,"hub"),string(&row,"listener_id"),&row["signal_id"],&format!("{host_id}:human"))?; }
                c.execute("UPDATE local_signals SET activity_hidden=1 WHERE hub=? AND signal_id=?", &[row["hub"].clone(),row["signal_id"].clone()])?;
                crate::core::state::reconcile_wake_retry(c, string(&row, "hub"), string(&row, "listener_id"))?;
                Ok(true)
            })?;
            Ok(())
        })?;
        Ok(())
    }
}
