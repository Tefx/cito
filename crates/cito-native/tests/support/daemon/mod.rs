use super::support::*;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Value};
use std::fs;

fn human() -> Value {
    serde_json::from_str(HUMAN).unwrap()
}
fn enroll(f: &Fixture, meta: Value) -> Value {
    if meta["expected_harness"] == "human" {
        let (status, result) = f.action(json!({"op":"cli_human_device","attach":true}));
        assert_eq!(status, 200, "{result}");
        return result;
    }
    f.tool(
        "cito_listen",
        json!({"about":"Native receiver","scope":"workspace"}),
        meta,
    )
}
// Human sends cross the real UI authority boundary, never MCP caller metadata.
fn send_human_result(f: &Fixture, mut args: Value) -> Value {
    args["op"] = json!("send_signal");
    let (status, mut result) = f.action(args);
    result["http_status"] = json!(status);
    result
}
fn send_human(f: &Fixture, args: Value) -> Value {
    let result = send_human_result(f, args);
    assert_eq!(result["http_status"], 200, "{result}");
    result
}
fn rpc(p: u16, op: &str, params: Value) -> Value {
    let mut v = params;
    v["op"] = json!(op);
    let (s, r) = http(p, "/v1/rpc", Some(&v));
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["ok"], true, "{r}");
    r["result"].clone()
}

mod config_and_hubs;
mod discovery_and_setup;
mod human_and_channels;
mod mcp_and_inbox;
