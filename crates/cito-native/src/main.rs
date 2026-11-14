use cito_native::core;
use cito_native::extract_jsonrpc_from_http_body;
pub use core::{daemon, hub, plugins, setup};

use plugins::{ensure_daemon, http_post_mcp, load_native_config, CITO_VERSION};
use serde_json::{json, Value};
use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

fn merge(target: &mut Value, patch: &Value) {
    if let (Some(t), Some(p)) = (target.as_object_mut(), patch.as_object()) {
        for (k, v) in p {
            if v.is_object() {
                merge(t.entry(k).or_insert_with(|| json!({})), v)
            } else {
                t.insert(k.clone(), v.clone());
            }
        }
    }
}
fn output(lock: &Mutex<()>, v: &Value) {
    if let Ok(_guard) = lock.lock() {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{v}");
        let _ = out.flush();
    }
}
struct Sidecar {
    child: Child,
    input: Option<ChildStdin>,
}
impl Drop for Sidecar {
    fn drop(&mut self) {
        self.input.take();
        let deadline = std::time::Instant::now() + Duration::from_millis(100);
        loop {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            if std::time::Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
static STDIO_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
extern "C" fn stdio_signal(_: libc::c_int) {
    STDIO_STOP.store(true, std::sync::atomic::Ordering::Relaxed);
}
fn pop_stdio_line(pending: &mut Vec<u8>) -> Result<Option<String>, String> {
    if let Some(end) = pending.iter().position(|b| *b == b'\n') {
        if end > 1_048_576 {
            return Err("MCP line exceeds 1 MiB".into());
        }
        let bytes = pending.drain(..=end).collect();
        return String::from_utf8(bytes)
            .map(Some)
            .map_err(|e| e.to_string());
    }
    if pending.len() > 1_048_576 {
        return Err("MCP line exceeds 1 MiB".into());
    }
    Ok(None)
}
fn read_sidecar_line(reader: &mut impl BufRead) -> std::io::Result<Option<String>> {
    let mut line = String::new();
    let n = reader.take(1_048_577).read_line(&mut line)?;
    if n > 1_048_576 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Sidecar line exceeds 1 MiB",
        ));
    }
    Ok((n > 0).then_some(line))
}
fn read_stdio_line(pending: &mut Vec<u8>) -> Option<String> {
    loop {
        if STDIO_STOP.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        match pop_stdio_line(pending) {
            Ok(Some(line)) => return Some(line),
            Ok(None) => {}
            Err(_) => return None,
        }
        let mut fd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll receives one valid descriptor structure; read writes only into the live byte array.
        let ready = unsafe { libc::poll(&mut fd, 1, 50) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return None;
        }
        if ready == 0 {
            continue;
        }
        let mut bytes = [0u8; 8192];
        let count =
            unsafe { libc::read(libc::STDIN_FILENO, bytes.as_mut_ptr().cast(), bytes.len()) };
        if count <= 0 {
            return None;
        }
        pending.extend_from_slice(&bytes[..count as usize]);
    }
}
fn run_mcp_stdio() {
    // SAFETY: handlers only store a lock-free atomic; owned sidecars are dropped by normal Rust control flow.
    unsafe {
        libc::signal(
            libc::SIGINT,
            stdio_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            stdio_signal as *const () as libc::sighandler_t,
        );
    }
    // SAFETY: ignoring SIGPIPE only changes signal disposition; write errors remain ordinary I/O errors.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let (config, plugins) = match load_native_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cito-mcp: {e}");
            std::process::exit(1)
        }
    };
    if let Err(e) = ensure_daemon(&config) {
        eprintln!("cito-mcp: {e}");
        std::process::exit(1)
    }
    let endpoint = env::var("CITO_ENDPOINT_PID")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|p| *p > 0)
        .unwrap_or_else(|| {
            // SAFETY: getppid has no pointer arguments or ownership effects.
            unsafe { libc::getppid() as u32 }
        });
    let lock = Arc::new(Mutex::new(()));
    let mut capabilities = json!({});
    let mut sidecars = vec![];
    for plugin in plugins.agent_plugins() {
        if !plugin
            .channel_env
            .as_ref()
            .and_then(|key| env::var(key).ok())
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false)
        {
            continue;
        }
        let result = (|| -> Result<(Sidecar, Value), String> {
            let mut cmd = plugins::plugin_command(plugin)?;
            let mut child = cmd
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|e| e.to_string())?;
            let mut sidecar = Sidecar {
                input: child.stdin.take(),
                child,
            };
            let stdout = sidecar
                .child
                .stdout
                .take()
                .ok_or("Plugin stdout unavailable")?;
            let request = json!({"op":"stdio_sidecar","endpoint_pid":endpoint,"cwd":env::current_dir().ok(),"state_dir":config.state_dir,"env":env::vars().collect::<std::collections::BTreeMap<_,_>>()});
            writeln!(
                sidecar.input.as_mut().ok_or("Plugin stdin unavailable")?,
                "{request}"
            )
            .map_err(|e| e.to_string())?;
            let (tx, rx) = mpsc::channel();
            let out = lock.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                let ready = read_sidecar_line(&mut reader)
                    .map_err(|e| e.to_string())
                    .and_then(|line| line.ok_or_else(|| "Plugin exited before ready".to_owned()))
                    .and_then(|line| {
                        serde_json::from_str::<Value>(&line).map_err(|e| e.to_string())
                    });
                let _ = tx.send(ready);
                while let Ok(Some(line)) = read_sidecar_line(&mut reader) {
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        if v["jsonrpc"] == "2.0" && v.get("method").is_some() {
                            output(&out, &v)
                        }
                    }
                }
            });
            let ready = rx
                .recv_timeout(Duration::from_secs(10))
                .map_err(|e| e.to_string())??;
            if ready["op"] != "ready" {
                return Err("Plugin did not send ready".into());
            }
            Ok((sidecar, ready["capabilities_patch"].clone()))
        })();
        match result {
            Ok((child, patch)) => {
                sidecars.push(child);
                merge(&mut capabilities, &patch)
            }
            Err(e) => eprintln!("cito-mcp sidecar {}: {e}", plugin.id),
        }
    }
    let mut pending = Vec::new();
    while let Some(line) = read_stdio_line(&mut pending) {
        let Ok(mut req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = req.get("id").cloned();
        if req["method"] == "tools/call" {
            if !req["params"].is_object() {
                req["params"] = json!({})
            }
            if !req["params"]["_meta"].is_object() {
                req["params"]["_meta"] = json!({})
            }
            req["params"]["_meta"]["_stdio"] = json!({"endpoint_pid":endpoint});
        }
        // Probe each subsequent request before forwarding. A failed mutating
        // request is reported once and is never replayed after uncertain delivery.
        let result = load_native_config()
            .and_then(|(current, _)| {
                if current.state_dir != config.state_dir {
                    return Err("MCP state directory changed; restart the client".into());
                }
                ensure_daemon(&current)?;
                http_post_mcp(
                    current.daemon_port,
                    &req.to_string(),
                    Duration::from_secs(60),
                )
            })
            .and_then(|r| {
                extract_jsonrpc_from_http_body(&r.body)
                    .ok_or_else(|| format!("Invalid daemon response (HTTP {})", r.status))
            });
        if let Some(id) = id {
            match result {
                Ok(mut rpc) => {
                    if rpc["result"].get("protocolVersion").is_some() {
                        if !rpc["result"]["capabilities"].is_object() {
                            rpc["result"]["capabilities"] = json!({})
                        }
                        merge(&mut rpc["result"]["capabilities"], &capabilities)
                    }
                    output(&lock, &rpc)
                }
                Err(e) => output(
                    &lock,
                    &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":e}}),
                ),
            }
        }
    }
    drop(sidecars);
}
fn main() {
    let args: Vec<String> = env::args().collect();
    let argv0 = args
        .first()
        .and_then(|s| Path::new(s).file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("");
    match args.get(1).map(String::as_str) {
        Some("--version" | "-v") => println!("cito v{CITO_VERSION}"),
        Some("setup") => setup::run_setup_cli(&args[2..]),
        Some("hub") => hub::run_hub_cli(&args[2..]),
        Some("daemon") => {
            let (config, plugins) = load_native_config().unwrap_or_else(|e| {
                eprintln!("cito daemon: {e}");
                std::process::exit(1)
            });
            daemon::run_daemon(&config, &plugins);
        }
        Some("listen" | "unlisten" | "rename" | "status" | "find" | "send" | "inbox") => {
            let (config, plugins) = load_native_config().unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(1)
            });
            daemon::run_cli_command(&args[1], &args[2..], &config, &plugins);
        }
        Some("mcp-stdio") | None => {
            if argv0 == "Cito" && args.get(1).is_none() {
                let (config, plugins) = load_native_config().unwrap_or_else(|e| {
                    eprintln!("cito daemon: {e}");
                    std::process::exit(1)
                });
                daemon::run_daemon(&config, &plugins);
            } else {
                run_mcp_stdio()
            }
        }
        Some(other) => {
            eprintln!("cito-native: unknown subcommand '{other}'");
            std::process::exit(1)
        }
    }
}

#[cfg(test)]
mod framing_tests {
    use super::*;
    #[test]
    fn adjacent_valid_lines_do_not_share_the_line_budget() {
        let mut pending = vec![b' '; 1_048_576];
        pending.push(b'\n');
        pending.extend_from_slice(b"{}\n");
        assert_eq!(
            pop_stdio_line(&mut pending).unwrap().unwrap().len(),
            1_048_577
        );
        assert_eq!(
            pop_stdio_line(&mut pending).unwrap().as_deref(),
            Some("{}\n")
        );
        assert!(pop_stdio_line(&mut pending).unwrap().is_none());
        let mut oversized = vec![b'x'; 1_048_577];
        oversized.push(b'\n');
        assert!(pop_stdio_line(&mut oversized).is_err());
        let mut no_newline = vec![b'x'; 1_048_577];
        assert!(pop_stdio_line(&mut no_newline).is_err());
    }
    #[test]
    fn sidecar_lines_stop_at_the_budget() {
        let mut reader = std::io::Cursor::new(b"{}\n{}\n");
        assert_eq!(
            read_sidecar_line(&mut reader).unwrap().as_deref(),
            Some("{}\n")
        );
        assert_eq!(
            read_sidecar_line(&mut reader).unwrap().as_deref(),
            Some("{}\n")
        );
        assert!(read_sidecar_line(&mut reader).unwrap().is_none());
        assert!(read_sidecar_line(&mut std::io::Cursor::new(vec![b'x'; 1_048_577])).is_err());
    }
}
