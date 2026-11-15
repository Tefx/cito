#[path = "../../../plugins/claude/native.rs"]
mod claude;
pub use claude::{hook_main as run_claude_hook, main as run_claude_plugin};
pub mod core;
pub use core::{daemon, database, hub, plugins, setup};
mod oidc_constants;
#[path = "plugin_support.rs"]
pub mod support;

use serde_json::Value;

pub fn extract_jsonrpc_from_http_body(body: &str) -> Option<Value> {
    body.lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .rfind(|s| !s.trim().is_empty())
        .and_then(|s| serde_json::from_str(s).ok())
        .or_else(|| serde_json::from_str(body.trim()).ok())
}
// Generic UUID validation is also used by legacy CLI argument parsing.
pub fn is_valid_transcript_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}
