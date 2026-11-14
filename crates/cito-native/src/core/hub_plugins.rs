//! Persistent, generic HumanChannel NDJSON subprocess transport.
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct ChannelPlugin {
    pub id: String,
    pub dir: PathBuf,
    pub manifest: Value,
    pub command: Vec<String>,
}

pub fn discover(
    roots: &[(PathBuf, bool)],
    user_root: Option<&Path>,
) -> Result<Vec<ChannelPlugin>, String> {
    let mut plugins = Vec::new();
    let mut seen_ids = HashSet::new();
    if let Some(state) = user_root.and_then(Path::parent) {
        super::plugins::cleanup_retired_companion(state)?;
    }
    for (root, is_builtin) in roots {
        if !root.exists() {
            continue;
        }
        // A bundled fallback may share the user root's target without being a user copy.
        let is_user_root = user_root.is_some_and(|user| root == user);
        for entry in std::fs::read_dir(root)
            .map_err(|e| format!("PLUGIN_LOAD_ERROR: {}: {e}", root.display()))?
        {
            let dir = entry.map_err(|e| e.to_string())?.path();
            let path = dir.join("plugin.json");
            if !path.exists() {
                continue;
            }
            let dir_name = dir.file_name().and_then(|name| name.to_str()).unwrap_or("");
            if is_user_root && super::plugins::remove_legacy_staged_plugin(&dir, true) {
                continue;
            }
            if dir_name == "human" {
                continue;
            }
            if seen_ids.contains(dir_name) {
                continue;
            }
            let m: Value =
                serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| format!("PLUGIN_LOAD_ERROR: {}: {e}", path.display()))?;
            if m["id"].as_str() == Some("human") {
                continue;
            }
            let descriptor = super::plugins::parse_manifest(&dir, *is_builtin, &m)
                .map_err(|e| format!("PLUGIN_LOAD_ERROR: {}: {e}", path.display()))?;
            // A higher-priority manifest owns its ID even if it changes kind.
            if !seen_ids.insert(descriptor.id.clone()) {
                continue;
            }
            if descriptor.kind != "human-channel" || m["scope"] == "user" {
                continue;
            }
            let mut command = descriptor.command;
            if command[0].starts_with("./") || command[0].starts_with("../") {
                command[0] = dir.join(&command[0]).to_string_lossy().into();
            }
            plugins.push(ChannelPlugin {
                id: descriptor.id,
                dir,
                command,
                manifest: m,
            });
        }
    }
    plugins.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(plugins)
}

#[cfg(test)]
#[path = "../../tests/hub/hub_plugin_cases.rs"]
mod tests;

type Pending = Arc<Mutex<HashMap<String, mpsc::SyncSender<Value>>>>;
pub struct ChannelProcess {
    child: Mutex<Child>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    alive: Arc<AtomicBool>,
    pending: Pending,
    reader: Mutex<Option<thread::JoinHandle<()>>>,
}
impl ChannelProcess {
    pub fn spawn(
        plugin: &ChannelPlugin,
        callback: Arc<dyn Fn(Value) -> Option<Value> + Send + Sync>,
    ) -> Result<Self, String> {
        let mut child = Command::new(&plugin.command[0])
            .args(&plugin.command[1..])
            .current_dir(&plugin.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .process_group(0)
            .spawn()
            .map_err(|e| format!("PLUGIN_EXEC_FAILED: {}: {e}", plugin.id))?;
        let input = child
            .stdin
            .take()
            .ok_or("PLUGIN_EXEC_FAILED: missing stdin")?;
        use std::os::fd::AsRawFd;
        // SAFETY: input owns a live pipe descriptor. Nonblocking writes bound shutdown latency.
        unsafe {
            let fd = input.as_raw_fd();
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::last_os_error().to_string());
            }
        }
        let stdin = Arc::new(Mutex::new(Some(input)));
        let stdout = child
            .stdout
            .take()
            .ok_or("PLUGIN_EXEC_FAILED: missing stdout")?;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let (si, p, a) = (stdin.clone(), pending.clone(), alive.clone());
        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                // A broken plugin cannot grow a line indefinitely.
                use std::io::Read;
                match reader
                    .by_ref()
                    .take(8 * 1024 * 1024 + 1)
                    .read_until(b'\n', &mut line)
                {
                    Ok(0) | Err(_) => break,
                    _ if line.len() > 8 * 1024 * 1024 => break,
                    _ => {}
                }
                let Ok(frame) = serde_json::from_slice::<Value>(&line) else {
                    continue;
                };
                if frame["op"] == "rpc_result" {
                    if let Some(id) = frame["req_id"].as_str() {
                        if let Ok(mut map) = p.lock() {
                            if let Some(tx) = map.remove(id) {
                                let _ = tx.send(frame);
                            }
                        }
                    }
                } else if a.load(Ordering::SeqCst) {
                    if let Some(reply) = callback(frame) {
                        if send_line(&si, &reply).is_err() {
                            break;
                        }
                    }
                }
            }
            a.store(false, Ordering::SeqCst);
            if let Ok(mut si) = si.lock() {
                si.take();
            }
            if let Ok(mut p) = p.lock() {
                for (_, tx) in p.drain() {
                    let _ = tx.send(json!({"ok":false,"error":"Plugin subprocess exited"}));
                }
            }
        });
        Ok(Self {
            child: Mutex::new(child),
            stdin,
            alive,
            pending,
            reader: Mutex::new(Some(reader)),
        })
    }
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
    pub fn send(&self, value: &Value) -> Result<(), String> {
        if !self.alive() {
            return Err("PLUGIN_EXEC_FAILED: Plugin subprocess exited".into());
        }
        let result = send_line(&self.stdin, value);
        if result.is_err() {
            self.alive.store(false, Ordering::SeqCst);
            if let Ok(mut input) = self.stdin.lock() {
                input.take();
            }
        }
        result
    }
    pub fn request(&self, mut value: Value) -> Result<Value, String> {
        let mut bytes = [0u8; 16];
        getrandom::getrandom(&mut bytes).map_err(|e| e.to_string())?;
        let id = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        value["req_id"] = json!(id);
        let (tx, rx) = mpsc::sync_channel(1);
        self.pending
            .lock()
            .map_err(|_| "Plugin pending lock poisoned")?
            .insert(id.clone(), tx);
        if let Err(e) = self.send(&value) {
            if let Ok(mut p) = self.pending.lock() {
                p.remove(&id);
            }
            return Err(e);
        }
        let result = rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| "PLUGIN_EXEC_FAILED: plugin request timed out".to_owned());
        if let Ok(mut p) = self.pending.lock() {
            p.remove(&id);
        }
        result
    }
    pub fn shutdown(&self) {
        let Ok(mut reader) = self.reader.lock() else {
            return;
        };
        if reader.is_none() {
            return;
        }
        let _ = self.send(&json!({"op":"shutdown"}));
        // Closing stdin also triggers the protocol's plugin-side EOF lifecycle.
        if let Ok(mut stdin) = self.stdin.lock() {
            stdin.take();
        }
        self.alive.store(false, Ordering::SeqCst);
        if let Ok(mut pending) = self.pending.lock() {
            for (_, tx) in pending.drain() {
                let _ = tx.send(json!({"ok":false,"error":"Plugin subprocess exited"}));
            }
        }
        if let Ok(mut child) = self.child.lock() {
            let until = Instant::now() + Duration::from_millis(200);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Err(_) => break,
                    _ => {}
                }
                if Instant::now() >= until {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            // SAFETY: spawn gave this child its own process group. Kill descendants
            // too: inherited stdout pipes must close before joining the reader.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        // No cursor, pairing, or delivery callback may outlive a stopped process.
        // Callers release the process registry and database locks before shutdown.
        if let Some(handle) = reader.take() {
            if handle.thread().id() != thread::current().id() {
                let _ = handle.join();
            }
        }
    }
}
impl Drop for ChannelProcess {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn send_line(stdin: &Mutex<Option<ChildStdin>>, value: &Value) -> Result<(), String> {
    let mut guard = stdin.lock().map_err(|_| "Plugin stdin lock poisoned")?;
    let stdin = guard
        .as_mut()
        .ok_or("PLUGIN_EXEC_FAILED: Plugin subprocess exited")?;
    let mut bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    let mut offset = 0;
    let until = Instant::now() + Duration::from_secs(2);
    while offset < bytes.len() {
        match stdin.write(&bytes[offset..]) {
            Ok(0) => return Err("PLUGIN_EXEC_FAILED: closed stdin".into()),
            Ok(n) => offset += n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until => {
                thread::sleep(Duration::from_millis(2))
            }
            Err(e) => return Err(format!("PLUGIN_EXEC_FAILED: {e}")),
        }
    }
    stdin
        .flush()
        .map_err(|e| format!("PLUGIN_EXEC_FAILED: {e}"))
}
