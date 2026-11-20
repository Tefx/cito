//! Persistent, generic HumanChannel NDJSON subprocess transport.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
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
    pub is_builtin: bool,
}

pub fn discover(
    roots: &[(PathBuf, bool)],
    user_root: Option<&Path>,
) -> Result<Vec<ChannelPlugin>, String> {
    let mut plugins = Vec::new();
    if let Some(state) = user_root.and_then(Path::parent) {
        super::plugins::cleanup_retired_companion(state)?;
    }
    let external: Vec<_> = roots
        .iter()
        .filter(|(_, builtin)| !builtin)
        .map(|(path, _)| path.clone())
        .collect();
    let builtin: Vec<_> = roots
        .iter()
        .filter(|(_, builtin)| *builtin)
        .map(|(path, _)| path.clone())
        .collect();
    let loaded = super::plugins::load_plugins_from_roots(&external, &builtin, user_root, true);
    for descriptor in loaded.plugins {
        let dir = descriptor.plugin_dir;
        // Discovery owns validation and fallback selection. A concurrently removed
        // manifest affects only this entry, never the other channel services.
        let Some(m) = std::fs::read(dir.join("plugin.json"))
            .ok()
            .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
        else {
            continue;
        };
        if descriptor.kind != "human-channel" || m["scope"] == "user" {
            continue;
        }
        let command = descriptor.command;
        plugins.push(ChannelPlugin {
            id: descriptor.id,
            dir,
            command,
            manifest: m,
            is_builtin: descriptor.is_builtin,
        });
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
    shutdown_wake: Arc<crate::support::WakePipe>,
}
impl ChannelProcess {
    pub fn spawn(
        plugin: &ChannelPlugin,
        callback: Arc<dyn Fn(Value) -> Option<Value> + Send + Sync>,
    ) -> Result<Self, String> {
        let shutdown_wake = Arc::new(crate::support::WakePipe::new()?);
        let mut manifest = plugin.manifest.clone();
        manifest["id"] = json!(plugin.id);
        manifest["displayName"] = json!(plugin.id);
        manifest["command"] = json!(plugin.command);
        let descriptor = super::plugins::parse_manifest(&plugin.dir, plugin.is_builtin, &manifest)?;
        let mut child = super::plugins::plugin_command(&descriptor)?
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
        // SAFETY: stdout owns the pipe; nonblocking reads preserve partial lines
        // while the wake pipe can interrupt an inherited-but-idle stdout.
        unsafe {
            let flags = libc::fcntl(stdout.as_raw_fd(), libc::F_GETFL);
            if flags < 0
                || libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0
            {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::last_os_error().to_string());
            }
        }
        let exited = crate::support::child_exit_fd(child.id()).inspect_err(|_| {
            let _ = child.kill();
            let _ = child.wait();
        })?;
        let reader_wake = shutdown_wake.clone();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let (si, p, a) = (stdin.clone(), pending.clone(), alive.clone());
        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            while a.load(Ordering::SeqCst) {
                // A broken plugin cannot grow a line indefinitely.
                use std::io::Read;
                match reader
                    .by_ref()
                    .take(8 * 1024 * 1024 + 1)
                    .read_until(b'\n', &mut line)
                {
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if line.len() > 8 * 1024 * 1024 {
                            break;
                        }
                        let mut fds = [
                            libc::pollfd {
                                fd: reader.get_ref().as_raw_fd(),
                                events: libc::POLLIN,
                                revents: 0,
                            },
                            libc::pollfd {
                                fd: reader_wake.read_fd(),
                                events: libc::POLLIN,
                                revents: 0,
                            },
                            libc::pollfd {
                                fd: exited.as_raw_fd(),
                                events: libc::POLLIN,
                                revents: 0,
                            },
                        ];
                        if crate::support::poll_until(&mut fds, None).is_err()
                            || fds[1].revents != 0
                            || fds[2].revents != 0
                        {
                            break;
                        }
                        continue;
                    }
                    Ok(0) | Err(_) => break,
                    _ if line.len() > 8 * 1024 * 1024 => break,
                    _ => {}
                }
                let parsed = serde_json::from_slice::<Value>(&line);
                line.clear();
                let Ok(frame) = parsed else {
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
            shutdown_wake,
        })
    }
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
            && self
                .child
                .lock()
                .is_ok_and(|child| super::plugins::peek_plugin_exit(child.id()) == Ok(None))
    }
    pub fn send(&self, value: &Value) -> Result<(), String> {
        self.send_tracked(value).map_err(|(_, error)| error)
    }
    /// The boolean records whether any bytes reached the child pipe.
    pub(super) fn send_tracked(&self, value: &Value) -> Result<(), (bool, String)> {
        if !self.alive() {
            return Err((false, "PLUGIN_EXEC_FAILED: Plugin subprocess exited".into()));
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
        self.shutdown_wake.notify();
        if let Ok(mut pending) = self.pending.lock() {
            for (_, tx) in pending.drain() {
                let _ = tx.send(json!({"ok":false,"error":"Plugin subprocess exited"}));
            }
        }
        if let Ok(mut child) = self.child.lock() {
            // waitid also recognizes an exited child after its kqueue exit event
            // has passed. ECHILD prevents signaling after an external reap.
            let _ = crate::support::wait_child_exit(&child, Duration::from_millis(200));
            if super::plugins::peek_plugin_exit(child.id()).is_ok() {
                // SAFETY: the owned, unreaped child still reserves its PID and
                // process group, created by spawn. Never signal after reaping.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
            }
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
fn send_line(stdin: &Mutex<Option<ChildStdin>>, value: &Value) -> Result<(), (bool, String)> {
    let mut offset = 0;
    let result = (|| -> Result<(), String> {
        let mut guard = stdin.lock().map_err(|_| "Plugin stdin lock poisoned")?;
        let stdin = guard
            .as_mut()
            .ok_or("PLUGIN_EXEC_FAILED: Plugin subprocess exited")?;
        let mut bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        let until = Instant::now() + Duration::from_secs(2);
        while offset < bytes.len() {
            match stdin.write(&bytes[offset..]) {
                Ok(0) => return Err("PLUGIN_EXEC_FAILED: closed stdin".into()),
                Ok(n) => offset += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until => {
                    crate::support::poll_until(
                        &mut [libc::pollfd {
                            fd: stdin.as_raw_fd(),
                            events: libc::POLLOUT,
                            revents: 0,
                        }],
                        Some(until),
                    )?;
                    if Instant::now() >= until {
                        return Err("PLUGIN_EXEC_FAILED: channel input timed out".into());
                    }
                }
                Err(e) => return Err(format!("PLUGIN_EXEC_FAILED: {e}")),
            }
        }
        stdin
            .flush()
            .map_err(|e| format!("PLUGIN_EXEC_FAILED: {e}"))
    })();
    result.map_err(|e| (offset != 0, e))
}
