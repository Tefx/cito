//! Activity persistence transitions; HTTP/UI callers only select operations and project results.
use super::*;

fn optional_string<'a>(action: &'a Value, key: &str) -> Result<&'a str, String> {
    action.get(key).map_or(Ok(""), |value| {
        value
            .as_str()
            .ok_or_else(|| format!("INVALID_INPUT: {key} must be a string"))
    })
}

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
        let hub = optional_string(action, "hub")?;
        let query = optional_string(action, "query")?;
        let (filter, query) = if bulk {
            (
                action["filter"].as_str().unwrap_or(default_filter),
                query.trim().to_lowercase(),
            )
        } else {
            ("all", String::new())
        };
        // Only a text search needs the payload. Stream those rows through the same Unicode
        // and address matcher as the activity view rather than collecting search results.
        let search = if query.is_empty() {
            ""
        } else {
            ACTIVITY_SEARCH
        };
        self.db.transaction(|c| {
            let mut groups = std::collections::HashMap::<_,Vec<Value>>::new();
            c.query_each(&format!("SELECT s.hub,s.listener_id,s.signal_id,s.batch_id,l.harness AS target_harness,{ACTIVITY_ORIGIN}{search} FROM local_signals s JOIN local_listeners l ON l.hub=s.hub AND l.listener_id=s.listener_id WHERE s.activity_hidden=0 AND s.acknowledged_at_ms IS NULL AND (?1='' OR s.hub=?1) AND (?2!='me' OR l.harness='human')"), &[json!(hub),json!(filter)], |row| {
                let hub = string(&row,"hub");
                let id = string(&row,"signal_id");
                let selected = bulk || ids.iter().filter_map(Value::as_str).any(|key| key == id || Some(key) == row["batch_id"].as_str() || key.strip_prefix(hub).and_then(|s|s.strip_prefix(':')) == Some(id));
                if selected && joined_signal_is_current(&row) && (query.is_empty() || matches_activity(&row,&query)) {
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
        let selected = optional_string(action, "hub")?;
        let query = optional_string(action, "query")?.trim().to_lowercase();
        let search = if query.is_empty() {
            ""
        } else {
            ACTIVITY_SEARCH
        };
        self.db.transaction(|c| {
            let signal_id = action["signal_id"].as_str().unwrap_or("");
            let filter=action["filter"].as_str().unwrap_or(default_filter);
            let mut groups = HashMap::<_,Vec<_>>::new();
            c.query_each(&format!("SELECT s.hub,s.listener_id,s.signal_id,s.acknowledged_at_ms,l.harness AS target_harness,{ACTIVITY_ORIGIN}{search} FROM local_signals s LEFT JOIN local_listeners l ON l.hub=s.hub AND l.listener_id=s.listener_id WHERE s.activity_hidden=0 AND (?='' OR s.hub=?) AND (?='' OR signal_id=?) AND (?!='me' OR l.harness='human') AND (?!='unread' OR s.acknowledged_at_ms IS NULL)"), &[json!(selected),json!(selected),json!(signal_id),json!(signal_id),json!(filter),json!(filter)],|row| {
                if joined_signal_is_current(&row) && (query.is_empty() || matches_activity(&row,&query)) {
                    groups.entry((string(&row,"hub").to_owned(), string(&row,"listener_id").to_owned())).or_default().push((row["signal_id"].clone(), row["target_harness"] == "human" && row["acknowledged_at_ms"].is_null()));
                }
            })?;
            for ((hub,node), rows) in groups {
                for (id, ack) in rows {
                    if ack { acknowledge_current(c,&hub,&node,&id,&format!("{host_id}:human"))?; }
                    c.execute("UPDATE local_signals SET activity_hidden=1 WHERE hub=? AND signal_id=?", &[json!(hub),id])?;
                }
                reconcile_wake_retry(c, &hub, &node)?;
            }
            Ok(())
        })
    }
}
