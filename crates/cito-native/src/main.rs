use cito_native::{daemon, extract_jsonrpc_from_http_body, hub, plugins, setup};

use plugins::{ensure_daemon, http_post_mcp, load_native_config, CITO_VERSION};
use serde_json::{json, Value};
use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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
fn output(lock: &Mutex<()>, v: &Value) -> Result<(), String> {
    let _guard = lock.lock().map_err(|_| "Output lock poisoned")?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{v}")
        .and_then(|_| out.flush())
        .map_err(|e| e.to_string())
}
struct Sidecar {
    child: Child,
    input: Arc<Mutex<Option<ChildStdin>>>,
}
impl Drop for Sidecar {
    fn drop(&mut self) {
        if let Ok(mut input) = self.input.lock() {
            input.take();
        }
        let _ = cito_native::support::wait_child_exit(&self.child, Duration::from_millis(100));
        let _ = cito_native::support::teardown_child_group(&mut self.child);
    }
}
static STDIO_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
// Signal handlers stay installed until exit, so the corresponding pipe must live that long.
static STDIO_WAKE: std::sync::OnceLock<cito_native::support::WakePipe> = std::sync::OnceLock::new();
extern "C" fn stdio_signal(_: libc::c_int) {
    STDIO_STOP.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(wake) = STDIO_WAKE.get() {
        wake.notify();
    }
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
    if n > 0 && !line.ends_with('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "Sidecar closed before newline",
        ));
    }
    Ok((n > 0).then_some(line))
}
fn read_stdio_line(pending: &mut Vec<u8>, wake_fd: i32) -> Result<Option<String>, String> {
    loop {
        if STDIO_STOP.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(None);
        }
        if let Some(line) = pop_stdio_line(pending)? {
            return Ok(Some(line));
        }
        let mut fds = [
            libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: poll borrows initialized descriptors owned by run_mcp_stdio.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.to_string());
        }
        if STDIO_STOP.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(None);
        }
        if fds
            .iter()
            .any(|fd| fd.revents & (libc::POLLERR | libc::POLLNVAL) != 0)
        {
            return Err("MCP input poll failed".into());
        }
        let mut bytes = [0u8; 8192];
        // SAFETY: the writable buffer spans exactly the length passed to read.
        let count =
            unsafe { libc::read(libc::STDIN_FILENO, bytes.as_mut_ptr().cast(), bytes.len()) };
        if count < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.to_string());
        }
        if count == 0 {
            return if pending.is_empty() {
                Ok(None)
            } else {
                String::from_utf8(std::mem::take(pending))
                    .map(Some)
                    .map_err(|e| e.to_string())
            };
        }
        pending.extend_from_slice(&bytes[..count as usize]);
    }
}
fn daemon_rpc_response(body: &str, status: u16, id: &Value) -> Result<Value, String> {
    let body = extract_jsonrpc_from_http_body(body)
        .ok_or_else(|| format!("Invalid daemon response (HTTP {status})"))?;
    if body.get("jsonrpc").is_none() {
        let message = body["error"]
            .as_str()
            .or_else(|| body["message"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Daemon request failed (HTTP {status})"));
        return Ok(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":message}}));
    }
    Ok(body)
}

#[cfg(test)]
mod rpc_tests {
    use super::*;
    #[test]
    fn http_failures_preserve_jsonrpc_request_ids() {
        for id in [json!(37), json!("request"), Value::Null] {
            let rpc = daemon_rpc_response(r#"{"error":"Access denied"}"#, 403, &id).unwrap();
            assert_eq!(rpc["jsonrpc"], "2.0");
            assert_eq!(rpc["id"], id);
            assert_eq!(rpc["error"]["code"], -32603);
            assert_eq!(rpc["error"]["message"], "Access denied");
        }
        let original = json!({"jsonrpc":"2.0","id":"original","result":{}});
        assert_eq!(
            daemon_rpc_response(&original.to_string(), 200, &Value::Null).unwrap(),
            original
        );
    }
}

fn run_mcp_stdio() -> Result<(), String> {
    let pipe = cito_native::support::WakePipe::new()?;
    let wake = STDIO_WAKE.get_or_init(|| pipe);
    let (failures, failure) = mpsc::channel();
    // SAFETY: handlers store a lock-free atomic and write to the live self-pipe.
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
    let (config, plugins) = load_native_config()?;
    ensure_daemon(&config)?;
    let endpoint = cito_native::support::enclosing_endpoint_pid(
        env::var("CITO_ENDPOINT_PID")
            .ok()
            .and_then(|s| s.parse().ok()),
        // SAFETY: getppid only reads the process parent.
        unsafe { libc::getppid() as u32 },
    );
    let lock = Arc::new(Mutex::new(()));
    let initialized = Arc::new(AtomicBool::new(false));
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
            use std::os::{fd::AsRawFd, unix::process::CommandExt};
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut cmd = plugins::plugin_command(plugin)?;
            let mut child = cmd
                .process_group(0)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|e| e.to_string())?;
            let input = child.stdin.take();
            let mut sidecar = Sidecar {
                input: Arc::new(Mutex::new(None)),
                child,
            };
            let stdout = sidecar
                .child
                .stdout
                .take()
                .ok_or("Plugin stdout unavailable")?;
            let request = json!({"op":"stdio_sidecar","endpoint_pid":endpoint,"cwd":env::current_dir().ok(),"state_dir":config.state_dir,"env":env::vars().collect::<std::collections::BTreeMap<_,_>>()});
            let mut input = input.ok_or("Plugin stdin unavailable")?;
            let fd = input.as_raw_fd();
            // SAFETY: input owns this live descriptor for the entire bounded write.
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                    return Err(std::io::Error::last_os_error().to_string());
                }
            }
            let bytes = format!("{request}\n").into_bytes();
            let mut offset = 0;
            while offset < bytes.len() {
                if STDIO_STOP.load(std::sync::atomic::Ordering::Relaxed)
                    || std::time::Instant::now() >= deadline
                {
                    return Err("Plugin startup cancelled or timed out".into());
                }
                match input.write(&bytes[offset..]) {
                    Ok(0) => return Err("Plugin stdin closed".into()),
                    Ok(n) => offset += n,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        let mut fds = [
                            libc::pollfd {
                                fd,
                                events: libc::POLLOUT,
                                revents: 0,
                            },
                            libc::pollfd {
                                fd: wake.read_fd(),
                                events: libc::POLLIN,
                                revents: 0,
                            },
                        ];
                        cito_native::support::poll_until(&mut fds, Some(deadline))?;
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
            *sidecar
                .input
                .lock()
                .map_err(|_| "Plugin stdin lock poisoned")? = Some(input);
            let input = sidecar.input.clone();
            let initialized = initialized.clone();
            let (tx, rx) = mpsc::channel();
            let ready_wake = Arc::new(cito_native::support::WakePipe::new()?);
            let ready_notice = ready_wake.clone();
            let out = lock.clone();
            let failures = failures.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                let ready = read_sidecar_line(&mut reader)
                    .map_err(|e| e.to_string())
                    .and_then(|line| line.ok_or_else(|| "Plugin exited before ready".to_owned()))
                    .and_then(|line| {
                        serde_json::from_str::<Value>(&line).map_err(|e| e.to_string())
                    });
                let active = ready.as_ref().is_ok_and(|v| v["op"] == "ready");
                let sent = tx.send(ready);
                ready_notice.notify();
                if !active || sent.is_err() {
                    return;
                }
                loop {
                    let frame = match read_sidecar_line(&mut reader) {
                        Ok(Some(line)) => {
                            serde_json::from_str::<Value>(&line).map_err(|error| error.to_string())
                        }
                        Ok(None) => break,
                        Err(error) => Err(error.to_string()),
                    };
                    let v = match frame {
                        Ok(v) => v,
                        Err(error) => {
                            eprintln!("cito-mcp sidecar framing: {error}");
                            break;
                        }
                    };
                    let Some(method) = v["method"].as_str() else {
                        continue;
                    };
                    if v["jsonrpc"] != "2.0"
                        || (method == "notifications/claude/channel"
                            && !initialized.load(Ordering::Acquire))
                    {
                        continue;
                    }
                    if let Err(error) = output(&out, &v) {
                        let _ = failures.send(format!("MCP stdout write failed: {error}"));
                        STDIO_STOP.store(true, Ordering::Relaxed);
                        wake.notify();
                        break;
                    }
                }
                if let Ok(mut input) = input.lock() {
                    input.take();
                }
            });
            let mut fds = [
                libc::pollfd {
                    fd: ready_wake.read_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: wake.read_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            if !cito_native::support::poll_until(&mut fds, Some(deadline))?
                || STDIO_STOP.load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err("Plugin startup cancelled or timed out".into());
            }
            let ready = rx.try_recv().map_err(|error| error.to_string())??;
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
    while let Some(line) = read_stdio_line(&mut pending, wake.read_fd())? {
        let mut req = match serde_json::from_str::<Value>(&line) {
            Ok(req) if req.is_object() => req,
            result => {
                let (code, message) = if result.is_err() {
                    (-32700, "Parse error")
                } else {
                    (-32600, "Invalid Request")
                };
                output(
                    &lock,
                    &json!({"jsonrpc":"2.0","id":null,"error":{"code":code,"message":message}}),
                )?;
                continue;
            }
        };
        if req["jsonrpc"] == "2.0"
            && req["method"] == "notifications/initialized"
            && req.get("id").is_none()
        {
            initialized.store(true, Ordering::Release);
        }
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
        let result = load_native_config().and_then(|(current, _)| {
            if current.state_dir != config.state_dir {
                return Err("MCP state directory changed; restart the client".into());
            }
            ensure_daemon(&current)?;
            if STDIO_STOP.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("MCP transport stopped".into());
            }
            http_post_mcp(
                current.daemon_port,
                &req.to_string(),
                Duration::from_secs(60),
            )
        });
        // Notifications are forwarded too; only an absent id suppresses stdout.
        if let Some(id) = req.get("id") {
            match result.and_then(|r| daemon_rpc_response(&r.body, r.status, id)) {
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
            }?
        }
    }
    drop(sidecars);
    failure.try_recv().map_or(Ok(()), Err)
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
            let (config, plugins) =
                load_native_config().unwrap_or_else(|e| plugins::exit_cli_error("cito daemon", &e));
            daemon::run_daemon(&config, &plugins);
        }
        Some("listen" | "unlisten" | "rename" | "status" | "find" | "send" | "inbox") => {
            let (config, plugins) =
                load_native_config().unwrap_or_else(|e| plugins::exit_cli_error("cito", &e));
            daemon::run_cli_command(&args[1], &args[2..], &config, &plugins);
        }
        Some("mcp-stdio") | None => {
            if argv0 == "Cito" && args.get(1).is_none() {
                let (config, plugins) = load_native_config()
                    .unwrap_or_else(|e| plugins::exit_cli_error("cito daemon", &e));
                daemon::run_daemon(&config, &plugins);
            } else {
                run_mcp_stdio().unwrap_or_else(|e| plugins::exit_cli_error("cito-mcp", &e))
            }
        }
        Some(other) => {
            plugins::exit_cli_error("cito-native", &format!("unknown subcommand '{other}'"))
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
    fn invalid_poll_descriptor_is_an_input_error() {
        assert!(read_stdio_line(&mut Vec::new(), i32::MAX).is_err());
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
