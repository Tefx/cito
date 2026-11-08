use crate::plugins::{ensure_daemon, http_post_mcp, LoadedPlugins, NativeConfig};
use serde_json::{json, Map, Value};
use std::time::Duration;

fn read_value(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    if let Some((_, value)) = args[*i].split_once('=') {
        if value.is_empty() || value.starts_with('-') {
            return Err(format!("Missing value for {flag}"));
        }
        return Ok(value.into());
    }
    *i += 1;
    let value = args
        .get(*i)
        .filter(|s| !s.is_empty() && !s.starts_with('-'))
        .ok_or_else(|| format!("Missing value for {flag}"))?;
    Ok(value.clone())
}
pub fn parse_cli(
    cmd: &str,
    args: &[String],
    plugins: &LoadedPlugins,
) -> Result<(Value, Value), String> {
    let mut input = Map::new();
    let mut meta = Map::new();
    let mut positionals = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let raw = &args[i];
        let flag = raw.split('=').next().unwrap_or(raw);
        if let Some(plugin) = plugins
            .agent_plugins()
            .into_iter()
            .find(|p| p.cli_flag && flag == format!("--{}", p.id))
        {
            if let Some(old) = meta.get("expected_harness") {
                if old != &plugin.id {
                    return Err("Conflicting harness flags".into());
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
                meta.insert(
                    "expected_harness".into(),
                    json!(read_value(args, &mut i, flag)?),
                );
            }
            "--session" => {
                meta.insert(
                    "expected_session_id".into(),
                    json!(read_value(args, &mut i, flag)?),
                );
            }
            "--channel" => {
                meta.insert("channel".into(), json!(read_value(args, &mut i, flag)?));
            }
            "--scope" if cmd == "listen" => {
                let value = read_value(args, &mut i, flag)?;
                if !matches!(value.as_str(), "self" | "workspace") {
                    return Err("Invalid --scope".into());
                }
                input.insert("scope".into(), json!(value));
            }
            "--team" if cmd == "listen" => {
                if input.get("scope") == Some(&json!("self")) {
                    return Err("Conflicting --team and --scope self".into());
                }
                input.insert("scope".into(), json!("workspace"));
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
    } else if !positionals.is_empty() {
        return Err(format!("Unexpected argument: {}", positionals[0]));
    }
    if cmd == "send" && (!input.contains_key("to") || !input.contains_key("message")) {
        return Err("send requires --to and --message".into());
    }
    meta.insert(
        "_stdio".into(),
        json!({"endpoint_pid":std::env::var("CITO_ENDPOINT_PID")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or_else(|| unsafe { libc::getppid() as u32 })}),
    );
    meta.insert("cli_cmd".into(), json!(cmd));
    meta.insert(
        "cwd".into(),
        json!(std::env::current_dir().map_err(|e| e.to_string())?),
    );
    Ok((Value::Object(input), Value::Object(meta)))
}
pub fn run_cli_command(cmd: &str, args: &[String], config: &NativeConfig, plugins: &LoadedPlugins) {
    let result = (|| -> Result<(), String> {
        let (input, meta) = parse_cli(cmd, args, plugins)?;
        ensure_daemon(config)?;
        let body=json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":format!("cito_{cmd}"),"arguments":input,"_meta":meta}}).to_string();
        let response = http_post_mcp(config.daemon_port, &body, Duration::from_secs(30))?;
        let rpc = crate::extract_jsonrpc_from_http_body(&response.body)
            .ok_or_else(|| format!("HTTP {}", response.status))?;
        if let Some(error) = rpc.get("error") {
            return Err(error.to_string());
        }
        let text = rpc["result"]["content"][0]["text"]
            .as_str()
            .ok_or("Invalid daemon response")?;
        if rpc["result"]["isError"] == true {
            return Err(text.into());
        }
        let payload: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if args.iter().any(|a| a == "--json") || !matches!(cmd, "listen" | "unlisten") {
            println!("{text}");
        } else if cmd == "listen" {
            println!(
                "Listening as {} ({}): {}",
                payload["listener_id"].as_str().unwrap_or(""),
                payload["scope"].as_str().unwrap_or(""),
                payload["about"].as_str().unwrap_or("")
            );
        } else {
            println!(
                "Unlistened ({} removed)",
                payload["removed"].as_u64().unwrap_or(0)
            );
        }
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
        ] {
            assert!(parse_cli(
                cmd,
                &args.into_iter().map(str::to_string).collect::<Vec<_>>(),
                &p
            )
            .is_err());
        }
    }
}
