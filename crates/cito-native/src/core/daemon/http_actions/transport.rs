//! Local HTTP framing, MCP transport, and peer-process attribution.
use super::super::mcp::{tools_list, wait_send_receipt};
use super::super::ui_ipc::REQUEST_LIMIT;
use super::*;
use crate::plugins::CITO_VERSION;
use std::io::{BufReader, Read};
use std::net::TcpStream;

fn respond_bytes(stream: &mut TcpStream, status: u16, body: &[u8]) {
    let _ = crate::core::http_boundary::write_json(stream, status, body);
}

pub(in crate::core::daemon) fn respond(stream: &mut TcpStream, status: u16, value: &Value) {
    if let Ok(body) = serde_json::to_vec(value) {
        respond_bytes(stream, status, &body);
    }
}

fn http_client_pid(stream: &TcpStream) -> Option<u32> {
    let remote = stream.peer_addr().ok()?.port();
    let local = stream.local_addr().ok()?.port();
    let mut command = Command::new("/usr/sbin/lsof");
    command
        .args([
            "-nP",
            &format!("-iTCP:{remote}"),
            "-sTCP:ESTABLISHED",
            "-Fpn",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let output =
        crate::support::run_command_with_timeout(command, Duration::from_millis(250)).ok()??;
    let expected = format!("n127.0.0.1:{remote}->127.0.0.1:{local}");
    let mut pid = None;
    let mut matches = std::collections::HashSet::new();
    for line in std::str::from_utf8(&output.stdout).ok()?.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse::<u32>().ok();
        }
        if line == expected {
            if let Some(pid) = pid {
                matches.insert(pid);
            }
        }
    }
    matches.remove(&std::process::id());
    if matches.len() == 1 {
        matches.into_iter().next()
    } else {
        None
    }
}

pub(in crate::core::daemon) fn handle_http(
    mut stream: TcpStream,
    service: Arc<Mutex<DaemonService>>,
    health: Arc<Value>,
) {
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let Ok(copy) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(copy);
    let head = match crate::core::http_boundary::read_head(&mut reader, 16_384) {
        Ok(head) => head,
        Err((status, error)) => {
            respond(&mut stream, status, &json!({"error":error}));
            return;
        }
    };
    let method = head.method.as_str();
    let path = head.target.split('?').next().unwrap_or("");
    let length = head.length;
    let headers: Map<String, Value> = head
        .headers
        .into_iter()
        .map(|(key, value)| (key, json!(value)))
        .collect();
    let Ok(bound) = stream.local_addr() else {
        return;
    };
    if let Err((status, error)) = crate::core::http_boundary::validate(
        bound,
        method,
        headers.get("host").and_then(Value::as_str),
        headers.get("origin").and_then(Value::as_str),
        headers.get("content-type").and_then(Value::as_str),
    ) {
        respond(&mut stream, status, &json!({"error":error}));
        return;
    }
    if path == "/health" && method == "GET" {
        if shutdown_requested() {
            respond(
                &mut stream,
                503,
                &json!({"ok":false,"error":"Shutting down"}),
            );
        } else {
            respond(&mut stream, 200, &health);
        }
        return;
    }
    if method != "POST" {
        respond(&mut stream, 404, &json!({"error":"Not found"}));
        return;
    }
    if length > REQUEST_LIMIT {
        respond(
            &mut stream,
            413,
            &json!({"error":"Request body exceeds 1 MiB"}),
        );
        return;
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        respond(
            &mut stream,
            400,
            &json!({"error":"Incomplete request body"}),
        );
        return;
    }
    let Ok(mut parsed) = serde_json::from_slice::<Value>(&body) else {
        respond(&mut stream, 400, &json!({"error":"Invalid JSON"}));
        return;
    };
    if matches!(path, "/ui" | "/action" | "/control") {
        match dispatch_action(&service, &parsed, None) {
            Ok(v) => match bounded_frame(v) {
                Ok(bytes) => respond_bytes(&mut stream, 200, &bytes),
                Err(error) => respond(&mut stream, 500, &json!({"ok":false,"error":error})),
            },
            Err(e) => respond(&mut stream, 400, &json!({"ok":false,"error":e})),
        }
        return;
    }
    let Ok(mut s) = service.lock() else {
        return;
    };
    if s.closing || SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
        respond(&mut stream, 503, &json!({"error":"Shutting down"}));
        return;
    }
    if path != "/mcp" {
        respond(&mut stream, 404, &json!({"error":"Not found"}));
        return;
    }
    let id = parsed["id"].clone();
    if !parsed.is_object() || parsed["jsonrpc"] != "2.0" || !parsed["method"].is_string() {
        respond(
            &mut stream,
            400,
            &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32600,"message":"Invalid Request"}}),
        );
        return;
    }
    let method = string(&parsed, "method").to_string();
    if id.is_null() || method.starts_with("notifications/") {
        respond(&mut stream, 202, &Value::Null);
        return;
    }
    let result = match method.as_str() {
        "initialize" => {
            json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"cito","version":CITO_VERSION}})
        }
        "tools/list" => tools_list(),
        "ping" => json!({}),
        "tools/call" => {
            if !parsed["params"].is_object() {
                parsed["params"] = json!({});
            }
            let name = string(&parsed["params"], "name").to_string();
            if !parsed["params"]["_meta"].is_object() {
                parsed["params"]["_meta"] = json!({});
            }
            parsed["params"]["_meta"]["headers"] = Value::Object(headers);
            if let Some(pid) = http_client_pid(&stream) {
                parsed["params"]["_meta"]["_http_client"] = json!(pid);
            }
            let args = parsed["params"]["arguments"].clone();
            let tool = s.tool(&name, &args, &parsed["params"]["_meta"]);
            drop(s);
            match tool {
                Ok(mut payload) => {
                    if name == "cito_send" {
                        wait_send_receipt(&service, &mut payload);
                    }
                    json!({"isError":false,"content":[{"type":"text","text":payload.to_string()}]})
                }
                Err(error) => {
                    let (code, message) = crate::core::http_boundary::coded_error(&error);
                    json!({"isError":true,"content":[{"type":"text","text":json!({"ok":false,"error":{"code":code,"message":message.trim(),"retryable":matches!(code,"NATS_UNAVAILABLE"|"AUTH_UNAVAILABLE"|"STALE_GENERATION")}}).to_string()}]})
                }
            }
        }
        _ => {
            respond(
                &mut stream,
                200,
                &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}),
            );
            return;
        }
    };
    respond(
        &mut stream,
        200,
        &json!({"jsonrpc":"2.0","id":id,"result":result}),
    );
}
