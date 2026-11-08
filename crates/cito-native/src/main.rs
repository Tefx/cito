mod core;
mod daemon;
mod hub;
mod oidc_constants;
mod plugins;
mod setup;

use plugins::{ensure_daemon, http_post_mcp, load_native_config, CITO_VERSION};
use serde_json::{json, Value};
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

pub fn extract_jsonrpc_from_http_body(body: &str) -> Option<Value> {
    body.lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .filter(|s| !s.trim().is_empty())
        .last()
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
fn read_stdio_line(pending: &mut Vec<u8>) -> Option<String> {
    loop {
        if STDIO_STOP.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        if let Some(end) = pending.iter().position(|b| *b == b'\n') {
            let bytes: Vec<_> = pending.drain(..=end).collect();
            return String::from_utf8(bytes).ok();
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
        if pending.len() > 1_048_576 {
            return None;
        }
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
                let mut first = String::new();
                let ready = reader
                    .read_line(&mut first)
                    .map_err(|e| e.to_string())
                    .and_then(|n| {
                        if n == 0 {
                            Err("Plugin exited before ready".into())
                        } else {
                            serde_json::from_str::<Value>(&first).map_err(|e| e.to_string())
                        }
                    });
                let _ = tx.send(ready);
                for line in reader.lines().map_while(Result::ok) {
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
        let result = http_post_mcp(
            config.daemon_port,
            &req.to_string(),
            Duration::from_secs(60),
        )
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
        Some("listen" | "unlisten" | "find" | "send" | "inbox") => {
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
