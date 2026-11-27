use crate::core::{
    cli::companion_error,
    identity::string,
    plugins::account_home_dir,
    registration::{distinct_label, is_generic_session_label},
};
use serde_json::{json, Value};
use std::{collections::HashMap, path::Path};

fn diagnostics(row: &mut Value, keys: &[&str]) {
    for key in keys {
        if let Some(raw) = row[*key].as_str() {
            row[*key] = json!(companion_error(raw));
        }
    }
}

fn rows<'a>(frame: &'a mut Value, key: &str) -> impl Iterator<Item = &'a mut Value> {
    frame
        .get_mut(key)
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
}

fn basename(path: &Path) -> &str {
    path.file_name()
        .and_then(|part| part.to_str())
        .unwrap_or_default()
}

fn node_presentation(row: &mut Value, user: &str, process_home: &str, account_home: &str) {
    let title = string(row, "title");
    let about = string(row, "about");
    let human = row["harness"] == "human";
    let custom = row["custom_title"] == true;
    let displayed_title =
        if !custom && !human && is_generic_session_label(title, user, account_home) {
            distinct_label(about, title, user, process_home)
        } else {
            Some(title)
        };
    let human_directory_title = if human && !custom {
        Some(format!(
            "@{user}{}",
            if row["is_self_principal"] == true {
                " (You)"
            } else {
                ""
            }
        ))
    } else {
        None
    };
    let display_about = distinct_label(about, title, user, process_home).filter(|about| {
        let displayed = human_directory_title
            .as_deref()
            .or(displayed_title)
            .unwrap_or_default();
        about.to_lowercase() != displayed.trim().to_lowercase()
    });
    let use_app_title = displayed_title.is_none();
    let shown = json!(displayed_title.filter(|display| *display != title));
    let description = json!(display_about);
    if !shown.is_null() {
        row["display_title"] = shown;
    }
    if use_app_title {
        row["use_app_title"] = json!(true);
    }
    if !description.is_null() {
        row["display_about"] = description;
    }
}

/// Project human-readable fields on the local companion wire, without changing
/// the raw titles or the daemon's HTTP/MCP responses and stored state.
pub fn project_companion_frame(frame: &mut Value, process_home: &Path) {
    let account_home = account_home_dir();
    let account_home = account_home
        .as_deref()
        .map(Path::new)
        .map(basename)
        .unwrap_or_default();
    let process_home = basename(process_home);
    diagnostics(frame, &["warning"]);
    if frame["op"] == "channel_test_result" {
        // Keep the wire diagnostic verbatim; the companion displays detail first.
        if let Some(raw) = frame["detail"].as_str().or_else(|| frame["error"].as_str()) {
            frame["detail"] = json!(companion_error(raw));
        }
    } else {
        diagnostics(frame, &["error"]);
    }
    for row in rows(frame, "discovery") {
        diagnostics(row, &["error"]);
    }
    let hub_users: HashMap<_, _> = rows(frame, "hubs")
        .map(|hub| {
            diagnostics(hub, &["error"]);
            (
                string(hub, "name").to_owned(),
                hub["user"].as_str().unwrap_or("local").to_owned(),
            )
        })
        .collect();
    if let Some(settings) = frame
        .get_mut("settings")
        .filter(|settings| settings.is_object())
    {
        diagnostics(settings, &["channel_inventory_error"]);
        if let Some(host) = settings.get_mut("host_hub") {
            diagnostics(host, &["error"]);
        }
        for hub in rows(settings, "hubs") {
            diagnostics(hub, &["error"]);
        }
        for app in rows(settings, "apps") {
            diagnostics(app, &["detail"]);
        }
        for channel in rows(settings, "channels") {
            diagnostics(channel, &["detail"]);
        }
    }
    for row in rows(frame, "listeners") {
        let user = [
            row["user"].as_str(),
            row["owner_user"].as_str(),
            hub_users.get(string(row, "hub")).map(String::as_str),
        ]
        .into_iter()
        .flatten()
        .find(|user| !user.is_empty())
        .unwrap_or("local")
        .to_owned();
        node_presentation(row, &user, process_home, account_home);
    }
    for key in ["my_remote_listeners", "peers"] {
        for row in rows(frame, key) {
            let user = string(row, "user").to_owned();
            node_presentation(row, &user, process_home, account_home);
        }
    }
    for row in rows(frame, "available") {
        if row["custom_title"] == true {
            continue;
        }
        let title = string(row, "title");
        let generic = is_generic_session_label(title, "", account_home);
        if generic {
            row["generic_title"] = json!(true);
        } else {
            let mut fallback_hubs: Vec<_> = hub_users
                .iter()
                .filter_map(|(hub, user)| {
                    is_generic_session_label(title, user, account_home).then_some(hub)
                })
                .collect();
            if !fallback_hubs.is_empty() {
                fallback_hubs.sort_unstable();
                row["title_fallback_hubs"] = json!(fallback_hubs);
            }
        }
    }
    for item in rows(frame, "activity") {
        item["display_from_title"] = json!(distinct_label(
            string(item, "from_title"),
            "",
            string(item, "from_user"),
            process_home
        ));
    }
}
