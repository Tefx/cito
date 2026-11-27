use crate::plugins::{ensure_daemon, http_post_mcp, LoadedPlugins, NativeConfig};
use serde_json::{json, Map, Value};
use std::time::Duration;

fn read_value(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    super::cli_value::read(args, i, flag, false).map(str::to_owned)
}
pub fn parse_cli(
    cmd: &str,
    args: &[String],
    plugins: &LoadedPlugins,
) -> Result<(Value, Value), String> {
    let mut input = Map::new();
    let mut meta = Map::new();
    let mut positionals = Vec::new();
    let mut team = false;
    let mut i = 0;
    while i < args.len() {
        let raw = &args[i];
        let flag = raw.split('=').next().unwrap_or(raw);
        if raw.contains('=')
            && matches!(
                flag,
                "--team" | "--all" | "--unread" | "--peek" | "--no-ack" | "--ack-all" | "--json"
            )
        {
            return Err(format!("{flag} does not accept a value"));
        }
        if let Some(plugin) = plugins.agent_plugins().into_iter().find(|p| {
            p.cli_flag
                && !super::plugins::reserved_cli_flag(flag.trim_start_matches('-'))
                && flag == format!("--{}", p.id)
        }) {
            if let Some(old) = meta.get("expected_harness") {
                if old != &plugin.id {
                    return Err("INVALID_INPUT: Conflicting harness flags".into());
                }
            }
            meta.insert("expected_harness".into(), json!(plugin.id));
            if raw.contains('=') {
                meta.insert(
                    "expected_session_id".into(),
                    json!(read_value(args, &mut i, flag)?),
                );
            } else if args.get(i + 1).is_some_and(|s| !s.starts_with('-'))
                && (matches!(cmd, "unlisten" | "inbox")
                    || crate::is_valid_transcript_uuid(&args[i + 1]))
            {
                i += 1;
                meta.insert("expected_session_id".into(), json!(args[i]));
            }
            i += 1;
            continue;
        }
        match flag {
            "--hub" => {
                input.insert("hub".into(), json!(read_value(args, &mut i, flag)?));
            }
            "--harness" => {
                let harness = read_value(args, &mut i, flag)?;
                if meta
                    .get("expected_harness")
                    .is_some_and(|old| old != &harness)
                {
                    return Err("INVALID_INPUT: Conflicting harness flags".into());
                }
                meta.insert("expected_harness".into(), json!(harness));
            }
            "--session" => {
                meta.insert(
                    "expected_session_id".into(),
                    json!(read_value(args, &mut i, flag)?),
                );
            }
            "--scope" if matches!(cmd, "listen" | "unlisten" | "inbox") => {
                let value = read_value(args, &mut i, flag)?;
                if !matches!(value.as_str(), "self" | "workspace") {
                    return Err("Invalid --scope".into());
                }
                if team && value == "self" {
                    return Err("Conflicting --team and --scope self".into());
                }
                input.insert("scope".into(), json!(value));
            }
            "--team" if matches!(cmd, "listen" | "unlisten" | "inbox") => {
                if input.get("scope") == Some(&json!("self")) {
                    return Err("Conflicting --team and --scope self".into());
                }
                team = true;
                input.insert("scope".into(), json!("workspace"));
            }
            "--about" if cmd == "inbox" => {
                input.insert("about".into(), json!(read_value(args, &mut i, flag)?));
            }
            "--about" if matches!(cmd, "listen" | "unlisten") => {
                return Err(
                    "INVALID_INPUT: Use cito inbox to change the Human inbox description".into(),
                )
            }
            "--title" | "-t" if cmd == "listen" => {
                input.insert("title".into(), json!(read_value(args, &mut i, flag)?));
            }
            "--title" | "-t" | "--tag" if cmd == "unlisten" => {
                return Err("INVALID_INPUT: Unlisten does not accept title or tag flags; use cito inbox to change Human inbox policy".into());
            }
            "--all" if cmd == "unlisten" => {
                input.insert("all".into(), json!(true));
            }
            "--tag" if cmd == "listen" => {
                input.insert("tag".into(), json!(read_value(args, &mut i, flag)?));
            }
            "--to" if cmd == "send" => {
                input.insert("to".into(), json!(read_value(args, &mut i, flag)?));
            }
            "--message" | "-m" if cmd == "send" => {
                input.insert("message".into(), json!(read_value(args, &mut i, flag)?));
            }
            "--reply-to" | "--in-reply-to" if cmd == "send" => {
                input.insert("in_reply_to".into(), json!(read_value(args, &mut i, flag)?));
            }
            "--idempotency-key" if cmd == "send" => {
                input.insert(
                    "idempotency_key".into(),
                    json!(read_value(args, &mut i, flag)?),
                );
            }
            "--wake" if cmd == "send" => {
                return Err(
                    "--wake is not supported by send; delivery follows the target's channel policy"
                        .into(),
                );
            }
            "--user" | "-u" if cmd == "find" => {
                input.insert("user".into(), json!(read_value(args, &mut i, flag)?));
            }
            "--host" | "--repo" | "--query" | "-q" if cmd == "find" => {
                let key = match flag {
                    "--host" => "host",
                    "--repo" => "repo",
                    _ => "query",
                };
                input.insert(key.into(), json!(read_value(args, &mut i, flag)?));
            }
            "--limit" if matches!(cmd, "find" | "inbox") => {
                let value = read_value(args, &mut i, flag)?;
                let number = value
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n > 0 && *n <= if cmd == "find" { 50 } else { 1000 })
                    .ok_or("Invalid --limit")?;
                input.insert("limit".into(), json!(number));
            }
            "--unread" | "--peek" | "--no-ack" if cmd == "inbox" => {
                input.insert("ack_all".into(), json!(false));
            }
            "--ack-all" if cmd == "inbox" => {
                input.insert("ack_all".into(), json!(true));
            }
            "--ack" if cmd == "inbox" => {
                let value = read_value(args, &mut i, flag)?;
                let list = input.entry("ack_ids").or_insert_with(|| json!([]));
                if let Some(list) = list.as_array_mut() {
                    list.push(json!(value));
                }
            }
            "--json" => {}
            _ if raw.starts_with('-') => return Err(format!("Unknown flag: {raw}")),
            _ => positionals.push(raw.clone()),
        }
        i += 1;
    }
    if meta.contains_key("expected_session_id") && !meta.contains_key("expected_harness") {
        return Err("--session requires --harness".into());
    }
    if let Some(h) = meta.get("expected_harness").and_then(Value::as_str) {
        if h != "human" && plugins.find_by_id(h).is_none() {
            return Err(format!("Unknown harness: {h}"));
        }
    }
    if matches!(cmd, "listen" | "unlisten")
        && meta.get("expected_harness") == Some(&json!("human"))
        && (input.contains_key("title")
            || input.contains_key("tag")
            || input.contains_key("scope")
            || !positionals.is_empty()
            || meta.contains_key("expected_session_id"))
    {
        return Err("INVALID_INPUT: Human device commands do not accept session selectors, policy flags or descriptions; use cito inbox".into());
    }
    // Human inbox policy is scoped to a selected Hub, independently of its visibility.
    if matches!(cmd, "listen" | "unlisten")
        && input.contains_key("hub")
        && input.contains_key("scope")
    {
        return Err("INVALID_INPUT: --hub cannot be combined with --scope or --team".into());
    }
    if cmd == "unlisten" && input.contains_key("scope") {
        return Err("INVALID_INPUT: Use cito inbox for scope policy".into());
    }
    if cmd == "find" {
        if let Some(h) = meta.remove("expected_harness") {
            input.insert("harness".into(), h);
        }
        if !positionals.is_empty() {
            input.insert("query".into(), json!(positionals.join(" ")));
        }
    } else if cmd == "listen" {
        input.insert(
            "about".into(),
            json!(if positionals.is_empty() {
                "CLI session".into()
            } else {
                positionals.join(" ")
            }),
        );
    } else if cmd == "inbox"
        && positionals.len() == 1
        && matches!(positionals[0].as_str(), "pause" | "resume")
    {
        input.insert("paused".into(), json!(positionals[0] == "pause"));
    } else if cmd == "rename" {
        if positionals.is_empty() {
            return Err("rename requires a title".into());
        }
        let title = positionals.join(" ");
        super::registration::validate_title(&title)?;
        input.insert("title".into(), json!(title));
    } else if !positionals.is_empty() {
        return Err(format!("Unexpected argument: {}", positionals[0]));
    }
    if cmd == "inbox"
        && inbox_policy(&Value::Object(input.clone()))
        && (!meta.is_empty()
            || input
                .keys()
                .any(|key| !matches!(key.as_str(), "hub" | "scope" | "about" | "paused")))
    {
        return Err("Inbox policy commands accept only --hub, --scope, --team and --about".into());
    }
    if let Some(title) = input.get("title").and_then(Value::as_str) {
        super::registration::validate_title(title)?;
    }
    if input.get("all") == Some(&json!(true))
        && (input
            .keys()
            .any(|key| !matches!(key.as_str(), "all" | "hub"))
            || !meta.is_empty())
    {
        return Err("--all cannot be combined with session selectors".into());
    }
    if cmd == "status" && (!input.is_empty() || !meta.is_empty()) {
        return Err("status accepts only --json".into());
    }
    if cmd == "send" && (!input.contains_key("to") || !input.contains_key("message")) {
        return Err("send requires --to and --message".into());
    }
    meta.insert(
        "_stdio".into(),
        json!({"endpoint_pid":crate::support::enclosing_endpoint_pid(
            std::env::var("CITO_ENDPOINT_PID").ok().and_then(|s|s.parse().ok()),
            // SAFETY: getppid only reads the process parent.
            unsafe { libc::getppid() as u32 })}),
    );
    meta.insert("cli_cmd".into(), json!(cmd));
    meta.insert(
        "cwd".into(),
        json!(std::env::current_dir().map_err(|e| e.to_string())?),
    );
    Ok((Value::Object(input), Value::Object(meta)))
}
pub(super) fn presentation_error(raw: &str) -> String {
    format_presentation_error(raw, false)
}

pub(super) fn companion_error(raw: &str) -> String {
    format_presentation_error(raw, true)
}

fn format_presentation_error(raw: &str, companion: bool) -> String {
    let protocol_code = |code: &str| {
        code.len() >= 3
            && (companion || !matches!(code, "HTTP" | "OIDC" | "TLS" | "DNS"))
            && ((code.starts_with(|c: char| c.is_ascii_uppercase())
                && code
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'))
                || code.bytes().all(|b| b.is_ascii_digit()))
    };
    let mut unwrapped = raw.trim().to_owned();
    loop {
        if unwrapped.starts_with("CONTROL_REVISION_CONFLICT:") {
            return "Inbox changed on another device. Review the current state and try again."
                .into();
        }
        if unwrapped.starts_with(['{', '[']) {
            let detail = serde_json::from_str::<Value>(&unwrapped)
                .ok()
                .and_then(|v| {
                    v["message"]
                        .as_str()
                        .or_else(|| v["error"]["message"].as_str())
                        .or_else(|| v["error"].as_str())
                        .map(str::to_owned)
                });
            match detail {
                Some(detail) if !detail.is_empty() && detail != unwrapped => {
                    unwrapped = detail;
                    continue;
                }
                _ => return "The request failed. Try again.".into(),
            }
        }
        let lower = unwrapped.to_ascii_lowercase();
        if [
            "sqlite_",
            "sqlite3 ",
            "sqlite3:",
            "unable to open database",
            "database is locked",
            "database disk image is malformed",
            "no such table:",
        ]
        .iter()
        .any(|p| lower.starts_with(p))
            || lower.contains("(sqlite ")
            || lower.contains(": no such table:")
        {
            return "App database is unavailable. Check the app and its data location.".into();
        }
        let Some((code, rest)) = unwrapped.split_once(':') else {
            break;
        };
        let label = code.starts_with(|c: char| c.is_ascii_lowercase())
            && code
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
        let wrapper = ["discovery rejected", "badge rejected", "error"]
            .contains(&code.to_ascii_lowercase().as_str());
        if !(rest.is_empty() || rest.starts_with(char::is_whitespace))
            || !(protocol_code(code) || label || wrapper)
        {
            break;
        }
        unwrapped = rest.trim_start().to_owned();
    }
    // Strip protocol tokens anywhere, preserving word boundaries and URL/time colons.
    let mut stripped = String::new();
    let mut chars = unwrapped.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_alphanumeric() || c == '_' {
            let mut word = String::from(c);
            while chars
                .peek()
                .is_some_and(|c| c.is_alphanumeric() || *c == '_')
            {
                word.push(chars.next().unwrap());
            }
            let mut after = chars.clone();
            let http_status = !companion
                && word.len() == 3
                && word.bytes().all(|b| b.is_ascii_digit())
                && stripped.ends_with(char::is_whitespace)
                && stripped
                    .trim_end()
                    .strip_suffix("HTTP")
                    .is_some_and(|prefix| {
                        prefix
                            .chars()
                            .next_back()
                            .is_none_or(|c| !c.is_alphanumeric() && c != '_')
                    });
            if protocol_code(&word)
                && !http_status
                && after.next() == Some(':')
                && after.peek().is_none_or(|c| c.is_whitespace())
            {
                chars.next();
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
            } else {
                stripped.push_str(&word);
            }
        } else {
            stripped.push(c);
        }
    }
    let mut message = stripped.as_str();
    if let Some((prefix, suffix)) = message.rsplit_once("(os error ") {
        if suffix
            .strip_suffix(')')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
        {
            message = prefix.trim_end();
        }
    }
    if !companion && message.starts_with("The local hub must use local://") {
        return message.into();
    }
    if message.starts_with("Hub URL must use") {
        return "Enter a Hub URL using HTTP or HTTPS, without credentials, query, or fragment."
            .into();
    }
    if message.starts_with("CITO_NATS_URL") {
        return "Enter a valid Hub URL (http://ip:4291 or https://host).".into();
    }
    if let Some(reason) = message.strip_prefix("Cannot bind host hub listener: ") {
        return format!("Cannot start sharing: {reason}");
    }
    let mut message = message.to_owned();
    for (token, copy) in [
        ("DISCONNECTED_RETAINED", "Temporarily offline"),
        ("UNLISTENED", "Not listening"),
        ("EXPIRED", "Offline"),
        ("ACTIVE", "Active"),
        ("local://", "Built-in on this Mac"),
        ("CITO_OIDC_ISSUER", "OIDC issuer URL"),
        ("CITO_OIDC_CLIENT_ID", "OIDC client ID"),
        ("CITO_HUB_TOKEN", "shared access token"),
        ("CITO_AUTH_MODE", "access mode"),
        (" (http://127.0.0.1 is allowed for tests)", ""),
    ] {
        if token.bytes().all(|b| b.is_ascii_uppercase() || b == b'_') {
            let mut replaced = String::new();
            let mut cursor = 0;
            for (start, _) in message.match_indices(token) {
                let end = start + token.len();
                let word = |c: char| c.is_alphanumeric() || c == '_';
                if message[..start]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !word(c))
                    && message[end..].chars().next().is_none_or(|c| !word(c))
                {
                    replaced.push_str(&message[cursor..start]);
                    replaced.push_str(copy);
                    cursor = end;
                }
            }
            replaced.push_str(&message[cursor..]);
            message = replaced;
        } else {
            message = message.replace(token, copy);
        }
    }
    if companion {
        for (start, _) in message
            .to_ascii_lowercase()
            .match_indices("channel inventory")
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            let end = start + "channel inventory".len();
            let word = |c: char| c.is_alphanumeric() || c == '_';
            if !message[..start].chars().next_back().is_some_and(word)
                && !message[end..].chars().next().is_some_and(word)
            {
                message.replace_range(start..end, "Channel list");
            }
        }
    }
    message
}
pub fn format_status(payload: &Value) -> String {
    use std::fmt::Write;
    let mut text = String::new();
    let label = |row: &Value| {
        payload["harnesses"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|p| p["id"] == row["harness"])
            .and_then(|p| p["display_name"].as_str())
            .unwrap_or_else(|| row["harness"].as_str().unwrap_or("Agent"))
            .to_owned()
    };
    let _ = writeln!(text, "LISTENING AGENT SESSIONS");
    let mut listeners = payload["listeners"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["harness"] != "human")
        .peekable();
    if listeners.peek().is_none() {
        let _ = writeln!(text, "  (none)");
    }
    for row in listeners {
        let _ = writeln!(
            text,
            "  {} · {} · {}",
            label(row),
            row["title"].as_str().unwrap_or("Session"),
            row["display_address"].as_str().unwrap_or("")
        );
    }
    let _ = writeln!(text, "AVAILABLE SESSIONS");
    let mut available = payload["available"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|candidate| {
            !candidate["listening_hubs"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|hub| hub == &payload["hub"])
        })
        .peekable();
    if available.peek().is_none() {
        let _ = writeln!(text, "  (none)");
    }
    let home = super::plugins::system_home_dir();
    let home_basename = std::path::Path::new(&home)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let user = payload["hubs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|hub| hub["name"] == payload["hub"])
        .and_then(|hub| hub["user"].as_str())
        .unwrap_or("");
    for row in available {
        let app = label(row);
        let title = row["title"].as_str().unwrap_or("").trim();
        let harness = row["harness"].as_str().unwrap_or("");
        let compact_id;
        let title = if title.is_empty()
            || (row["custom_title"] != true
                && (super::registration::is_generic_session_label(title, user, home_basename)
                    || title.eq_ignore_ascii_case(&app)
                    || title.eq_ignore_ascii_case(harness)))
        {
            compact_id = row["session_id"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(8)
                .collect::<String>();
            compact_id.as_str()
        } else {
            title
        };
        let cwd = row["cwd"].as_str().unwrap_or("");
        let compact_cwd = std::path::Path::new(cwd)
            .strip_prefix(&home)
            .ok()
            .map(|path| {
                if path.as_os_str().is_empty() {
                    "~".into()
                } else {
                    format!("~/{}", path.display())
                }
            });
        let _ = writeln!(
            text,
            "  {} · {} · {}{}",
            app,
            title,
            compact_cwd.as_deref().unwrap_or(cwd),
            if row["needs_reload"] == true {
                " · Reload the app integration to listen"
            } else {
                ""
            }
        );
    }
    let _ = writeln!(text, "HUMAN INBOXES");
    for hub in payload["hubs"].as_array().into_iter().flatten() {
        let device = payload["listeners"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|r| r["harness"] == "human" && r["hub"] == hub["name"]);
        let status = if hub["human_state_status"] == "unknown" {
            "Unknown"
        } else if hub["human_paused"] == true {
            "Paused"
        } else {
            match hub["human_availability"].as_str() {
                Some("ACTIVE") => "Active",
                Some("DISCONNECTED_RETAINED") => "Reconnecting",
                _ => "Unavailable",
            }
        };
        let _ = writeln!(
            text,
            "  {} · {} · This device {}{}",
            hub["name"].as_str().unwrap_or("Hub"),
            status,
            if device { "attached" } else { "detached" },
            if hub["human_state_status"] == "stale" {
                " · Cached state"
            } else {
                ""
            }
        );
    }
    let _ = writeln!(text, "HUBS");
    for hub in payload["hubs"].as_array().into_iter().flatten() {
        let status = match hub["status"].as_str() {
            Some("connected") => "Connected",
            Some("connecting") => "Connecting",
            Some("auth_required") => "Sign in required",
            Some("disconnected") => "Disconnected",
            _ => "Unavailable",
        };
        let error = hub["error"]
            .as_str()
            .map(presentation_error)
            .unwrap_or_default();
        let _ = writeln!(
            text,
            "  {} · {}{}",
            hub["name"].as_str().unwrap_or("Hub"),
            status,
            if error.is_empty() {
                String::new()
            } else {
                format!(" · {error}")
            }
        );
    }
    text
}
pub fn control_request(config: &NativeConfig, action: &Value) -> Result<Value, String> {
    let response = crate::plugins::http_post_json(
        config.daemon_port,
        "/control",
        &action.to_string(),
        Duration::from_secs(30),
    )?;
    let value: Value = serde_json::from_str(&response.body).map_err(|e| e.to_string())?;
    if response.status != 200 || value["ok"] == false {
        return Err(value["error"].as_str().unwrap_or(&response.body).to_owned());
    }
    Ok(value)
}

fn inbox_policy(input: &Value) -> bool {
    ["paused", "scope", "about"]
        .iter()
        .any(|key| input.get(*key).is_some())
}

fn format_receipt(cmd: &str, payload: &Value) -> String {
    use std::fmt::Write;
    let string = |key: &str| payload[key].as_str().unwrap_or("");
    let count = |key: &str| payload[key].as_u64().unwrap_or(0);
    let address = || {
        payload["display_address"]
            .as_str()
            .unwrap_or_else(|| string("listener_id"))
    };
    match cmd {
        "status" => format_status(payload),
        "listen" => {
            let mut output = format!("Listening as {} ({})", address(), string("scope"));
            let about = string("about");
            if !about.is_empty() {
                let _ = write!(output, ": {about}");
            }
            output.push('\n');
            output
        }
        "unlisten" => format!("Unlistened ({} removed)\n", count("removed")),
        "rename" => format!("Renamed session to {}\n", string("title")),
        "send" => format!("Sent to {} ({})\n", string("to"), string("signal_id")),
        "find" => {
            let mut output = format!(
                "{} listeners ({} online)\n",
                count("matched_count"),
                count("online_count")
            );
            for row in payload["listeners"].as_array().into_iter().flatten() {
                if let Some(address) = row["display_address"].as_str() {
                    let _ = writeln!(output, "  {address}");
                }
            }
            output
        }
        "inbox" => {
            if let Some(signals) = payload["signals"].as_array() {
                let mut output = format!(
                    "{} signals ({} acknowledged)\n",
                    signals.len(),
                    count("acknowledged_count")
                );
                for signal in signals {
                    let from = &signal["from"];
                    let sender = from["title"]
                        .as_str()
                        .or_else(|| from["user"].as_str())
                        .unwrap_or("Unknown sender");
                    let _ = writeln!(
                        output,
                        "  {sender} ({})",
                        signal["signal_id"].as_str().unwrap_or("")
                    );
                    if let Some(message) = signal["message"].as_str() {
                        for line in message.lines() {
                            let _ = writeln!(output, "    {line}");
                        }
                    }
                }
                if payload["replay_gap_detected"] == true {
                    output.push_str("Warning: Some earlier signals may be missing\n");
                }
                output
            } else if let Some(record) = payload["record"].as_object() {
                let status = if record.get("human_paused") == Some(&Value::Bool(true)) {
                    "paused"
                } else {
                    "active"
                };
                let scope = record
                    .get("scope")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown scope");
                format!("Human inbox {status} ({scope})\n")
            } else {
                "Human inbox updated\n".into()
            }
        }
        _ => format!("{payload}\n"),
    }
}

fn receipt_warnings(payload: &Value) -> Vec<String> {
    let mut warnings = Vec::new();
    for row in payload["warnings"].as_array().into_iter().flatten() {
        if let Some(message) = row.as_str().or_else(|| row["message"].as_str()) {
            let message = presentation_error(message);
            let warning = match row["hub"].as_str() {
                Some(hub) => format!("{hub}: {message}"),
                None => message,
            };
            if !warning.is_empty() && !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
    }
    // A top-level warning is the aggregate when a detailed list is present.
    let aggregate = warnings
        .is_empty()
        .then(|| payload["warning"].as_str())
        .flatten();
    for message in [aggregate, payload["maintenance_warning"].as_str()]
        .into_iter()
        .flatten()
        .map(presentation_error)
    {
        if !message.is_empty() && !warnings.contains(&message) {
            warnings.push(message);
        }
    }
    warnings
}

fn terminal_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

pub fn run_cli_command(cmd: &str, args: &[String], config: &NativeConfig, plugins: &LoadedPlugins) {
    let result = (|| -> Result<(), String> {
        let (input, meta) = parse_cli(cmd, args, plugins)?;
        ensure_daemon(config)?;
        let payload = if cmd == "inbox" && inbox_policy(&input) {
            let mut action = input.clone();
            action["op"] = json!("human_inbox_update");
            control_request(config, &action)?
        } else if matches!(cmd, "listen" | "unlisten") && meta["expected_harness"] == "human" {
            let mut action = json!({"op":"cli_human_device","attach":cmd=="listen"});
            if let Some(hub) = input.get("hub") {
                action["hub"] = hub.clone();
            }
            control_request(config, &action)?
        } else if cmd == "rename" || cmd == "status" || input["all"] == true {
            let action = match cmd {
                "rename" => json!({"op":"cli_rename","title":input["title"],"meta":meta}),
                "status" => json!({"op":"get_snapshot"}),
                _ => {
                    let mut action = json!({"op":"unlisten_all"});
                    if let Some(hub) = input.get("hub") {
                        action["hub"] = hub.clone();
                    }
                    action
                }
            };
            control_request(config, &action)?
        } else {
            let body=json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":format!("cito_{cmd}"),"arguments":input,"_meta":meta}}).to_string();
            let response = http_post_mcp(config.daemon_port, &body, Duration::from_secs(30))?;
            let rpc = crate::extract_jsonrpc_from_http_body(&response.body)
                .ok_or_else(|| format!("HTTP {}", response.status))?;
            if let Some(error) = rpc.get("error") {
                return Err(crate::support::response_diagnostic(
                    &json!({"error":error}),
                    "The request failed",
                ));
            }
            let text = rpc["result"]["content"][0]["text"]
                .as_str()
                .ok_or("Invalid daemon response")?;
            if rpc["result"]["isError"] == true {
                return Err(serde_json::from_str::<Value>(text)
                    .map(|v| crate::support::response_diagnostic(&v, "The request failed"))
                    .unwrap_or_else(|_| text.to_owned()));
            }
            serde_json::from_str(text).map_err(|e| e.to_string())?
        };
        if args.iter().any(|a| a == "--json") {
            println!("{payload}");
        } else {
            print!("{}", terminal_text(&format_receipt(cmd, &payload)));
            for warning in receipt_warnings(&payload) {
                eprintln!("Warning: {}", terminal_text(&warning));
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        if args.iter().any(|a| a == "--json") {
            eprintln!("{e}");
        } else {
            eprintln!("{}", presentation_error(&e));
        }
        std::process::exit(1);
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_formats_readable_available_sessions_for_the_selected_hub() {
        let home = super::super::plugins::system_home_dir();
        let output = format_status(&json!({
            "hub":"work", "harnesses":[{"id":"editor","display_name":"Editor"}],
            "listeners":[{"harness":"human","hub":"work"}],
            "available":[
                {"harness":"editor","title":"Review routing","session_id":"visible-id","cwd":format!("{home}/service"),"listening_hubs":["personal"]},
                {"harness":"editor","title":"Session","session_id":"baadf00d-cafe-47b1-beca-238e0bd64e9d","cwd":format!("{home}-other/service"),"needs_reload":true},
                {"harness":"editor","title":"Editor","session_id":"pid-24","cwd":home},
                {"harness":"editor","title":"Already listening","listening_hubs":["work"]}
            ]
        }));
        assert_eq!(
            output,
            format!("LISTENING AGENT SESSIONS\n  (none)\nAVAILABLE SESSIONS\n  Editor · Review routing · ~/service\n  Editor · baadf00d · {home}-other/service · Reload the app integration to listen\n  Editor · pid-24 · ~\nHUMAN INBOXES\nHUBS\n")
        );
        let empty = format_status(&json!({"hub":"work","available":[{"listening_hubs":["work"]}]}));
        assert!(
            empty.starts_with("LISTENING AGENT SESSIONS\n  (none)\nAVAILABLE SESSIONS\n  (none)\n")
        );
    }

    #[test]
    fn explicit_and_plugin_harness_selectors_conflict_in_both_orders() {
        let descriptor = super::super::plugins::parse_manifest(
            std::path::Path::new("/tmp/editor"),
            false,
            &json!({"id":"editor","displayName":"Editor","cliFlag":true,"command":["./driver"]}),
        )
        .unwrap();
        let plugins = LoadedPlugins {
            plugins: vec![descriptor],
            rejected_ids: Default::default(),
        };
        for args in [
            vec!["--editor", "--harness", "human"],
            vec!["--harness", "human", "--editor"],
        ] {
            let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert!(parse_cli("listen", &args, &plugins)
                .unwrap_err()
                .starts_with("INVALID_INPUT: Conflicting harness"));
        }
        let args = ["--editor", "--harness", "editor"].map(str::to_owned);
        assert!(parse_cli("listen", &args, &plugins).is_ok());
    }
    #[test]
    fn human_device_policy_rejection_distinguishes_generated_description() {
        let plugins = LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        };
        for cmd in ["listen", "unlisten"] {
            for extra in [
                vec!["--title", "Title"],
                vec!["--tag", "review"],
                vec!["--scope", "workspace"],
                vec!["--team"],
                vec!["Description"],
                vec!["--about", "Description"],
                vec!["--session", "default"],
                vec!["--session", "session-a"],
            ] {
                let args = [vec!["--harness", "human"], extra]
                    .concat()
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                let error = parse_cli(cmd, &args, &plugins).unwrap_err();
                assert!(
                    error.starts_with("INVALID_INPUT:") && error.contains("cito inbox"),
                    "{error}"
                );
            }
        }
        assert!(parse_cli("listen", &["--harness".into(), "human".into()], &plugins).is_ok());
        let formatted = format_status(
            &json!({"listeners":[{"harness":"human","hub":"work"},{"harness":"agent","title":"Review","display_address":"alice/agent#abc"}],"harnesses":[{"id":"agent","display_name":"Editor"}],"available":[{"harness":"agent","session_id":"pending","cwd":"/repo","needs_reload":true}],"hubs":[{"name":"work","status":"auth_required","human_state_status":"current","human_paused":true,"error":"work: UNAUTHORIZED: Sign in required (os error 1)"}]}),
        );
        assert!(formatted.contains("Editor · Review"));
        assert!(formatted.contains("Reload the app integration"));
        assert!(formatted.contains("Paused · This device attached"));
        assert!(formatted.contains("Sign in required"));
        assert!(!formatted.contains("UNAUTHORIZED"));
        assert_eq!(
            presentation_error("work: FAILED: Detail: ACTIVE on local://"),
            "Detail: Active on Built-in on this Mac"
        );
        assert_eq!(
            presentation_error("Cannot bind host hub listener: Address in use (os error 48)"),
            "Cannot start sharing: Address in use"
        );
        assert_eq!(
            presentation_error("CONTROL_REVISION_CONFLICT: stale"),
            "Inbox changed on another device. Review the current state and try again."
        );
        assert!(
            !formatted.contains("UNAUTHORIZED")
                && !formatted.contains("os error")
                && !formatted.contains("auth_required")
        );
    }
    #[test]
    fn diagnostics_use_word_boundaries_and_strip_protocol_wrappers() {
        for (input,expected) in [
            ("INACTIVE EXPIRED_UNSENT ACTIVE EXPIRED","INACTIVE EXPIRED_UNSENT Active Offline"),
            ("work: PLUGIN_EXEC_FAILED: fixture: Discovery rejected: OFFLINE: Retry; AUTH_FAILED: Sign in","Retry; Sign in"),
            ("10071: 10037: Conflict","Conflict"),
            ("Badge rejected: BADGE_UNAVAILABLE: Retry later","Retry later"),
            (r#"{"ok":false,"error_code":"OFFLINE","message":"Retry later"}"#,"Retry later"),
            ("SQLITE_CANTOPEN: unable to open database file (SQLite 14)","App database is unavailable. Check the app and its data location."),
            ("unknown subcommand 'SQLITE_ERROR: no such table: private_name'", "App database is unavailable. Check the app and its data location."),
        ] { assert_eq!(presentation_error(input),expected,"{input}"); }
        let vectors: Vec<[String; 2]> =
            serde_json::from_str(include_str!("../../tests/support/diagnostics.json")).unwrap();
        for [input, expected] in vectors {
            assert_eq!(presentation_error(&input), expected, "{input}");
        }
    }
    #[test]
    fn diagnostics_preserve_operator_details() {
        for message in [
            "HTTP 409: Conflict",
            "Hub returned HTTP 409: Conflict",
            "Hub returned an error (HTTP 409: Conflict)",
            "OIDC: Issuer rejected the request",
            "TLS: Certificate verification failed",
            "DNS: Could not resolve the Hub",
            "The local hub must use local://",
            "Unknown harness: sqlite3",
        ] {
            assert_eq!(presentation_error(message), message);
            assert_eq!(
                presentation_error(&format!("work: FAILED: {message}")),
                message
            );
            assert_eq!(
                presentation_error(&json!({"error":{"message":message}}).to_string()),
                message
            );
        }
    }
    #[test]
    fn inbox_policy_and_boolean_flags_are_unambiguous() {
        let plugins = LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        };
        let (input, _) = parse_cli("inbox", &["pause".into()], &plugins).unwrap();
        assert_eq!(input["paused"], true);
        for extra in ["--unread", "--harness=human"] {
            assert!(parse_cli("inbox", &["pause".into(), extra.into()], &plugins).is_err());
        }
        for (cmd, flag) in [
            ("inbox", "--team"),
            ("unlisten", "--all"),
            ("inbox", "--unread"),
            ("inbox", "--peek"),
            ("inbox", "--no-ack"),
            ("inbox", "--ack-all"),
            ("status", "--json"),
        ] {
            assert!(parse_cli(cmd, &[format!("{flag}=false")], &plugins).is_err());
        }
        let (input, _) = parse_cli(
            "send",
            &["--to=alice".into(), "--message=-value".into()],
            &plugins,
        )
        .unwrap();
        assert_eq!(input["message"], "-value");
    }

    #[test]
    fn hub_and_scope_selectors_conflict_in_both_orders() {
        let plugins = LoadedPlugins {
            plugins: vec![],
            rejected_ids: Default::default(),
        };
        for cmd in ["listen", "unlisten"] {
            for flags in [
                ["--hub=work", "--scope=self"],
                ["--scope=self", "--hub=work"],
                ["--hub=work", "--team"],
                ["--team", "--hub=work"],
            ] {
                assert!(parse_cli(cmd, &flags.map(str::to_owned), &plugins)
                    .unwrap_err()
                    .contains("--hub cannot be combined"));
            }
        }
        let flags = ["resume", "--hub", "work", "--team"].map(str::to_owned);
        let (input, _) = parse_cli("inbox", &flags, &plugins).unwrap();
        assert_eq!(input["scope"], "workspace");
        assert_eq!(input["hub"], "work");
    }

    #[test]
    fn human_receipts_use_addresses_and_preserve_committed_warnings() {
        let find = json!({"matched_count":3,"online_count":2,"listeners":[
            {"display_address":"alice/agent#review","listener_id":"internal-id"}
        ]});
        assert_eq!(
            format_receipt("find", &find),
            "3 listeners (2 online)\n  alice/agent#review\n"
        );
        assert_eq!(
            format_receipt("send", &json!({"to":"alice","signal_id":"signal-a"})),
            "Sent to alice (signal-a)\n"
        );
        assert_eq!(
            format_receipt("rename", &json!({"title":"Review"})),
            "Renamed session to Review\n"
        );
        let inbox = json!({"signals":[{"signal_id":"signal-a","from":{"user":"alice"},"message":"Review\nReady"}],"acknowledged_count":1});
        assert_eq!(
            format_receipt("inbox", &inbox),
            "1 signals (1 acknowledged)\n  alice (signal-a)\n    Review\n    Ready\n"
        );
        let receipt = json!({"ok":true,"committed":true,"removed":1,"warning":"Aggregate",
            "warnings":[{"hub":"work","message":"UNAVAILABLE: Reconciliation pending"}],"maintenance_warning":"Cleanup pending"});
        assert_eq!(
            format_receipt("unlisten", &receipt),
            "Unlistened (1 removed)\n"
        );
        assert_eq!(
            receipt_warnings(&receipt),
            ["work: Reconciliation pending", "Cleanup pending"]
        );
        assert_eq!(
            receipt_warnings(
                &json!({"warning":"Cleanup pending","maintenance_warning":"Cleanup pending"})
            ),
            ["Cleanup pending"]
        );
    }

    #[test]
    fn flag_errors_precede_effects() {
        let p = LoadedPlugins {
            plugins: Vec::new(),
            rejected_ids: Default::default(),
        };
        for (cmd, args) in [
            ("send", vec!["--to", "--message", "x"]),
            ("find", vec!["--limit", "bad"]),
            ("inbox", vec!["--unknown"]),
            ("listen", vec!["--session", "s"]),
            ("listen", vec!["--title", " "]),
            ("listen", vec!["--team", "--scope", "self"]),
            ("listen", vec!["--scope", "self", "--team"]),
            ("rename", vec![]),
            ("rename", vec![" "]),
        ] {
            assert!(parse_cli(
                cmd,
                &args.into_iter().map(str::to_string).collect::<Vec<_>>(),
                &p
            )
            .is_err());
        }
        for (cmd, args, key, expected) in [
            (
                "listen",
                vec!["-t", "Backend API", "about"],
                "title",
                json!("Backend API"),
            ),
            (
                "rename",
                vec!["Backend API", "--json"],
                "title",
                json!("Backend API"),
            ),
            ("unlisten", vec!["--all"], "all", json!(true)),
            (
                "unlisten",
                vec!["--all", "--hub", "work"],
                "hub",
                json!("work"),
            ),
        ] {
            let (input, _) = parse_cli(
                cmd,
                &args.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                &p,
            )
            .unwrap();
            assert_eq!(input[key], expected);
        }
    }
}
