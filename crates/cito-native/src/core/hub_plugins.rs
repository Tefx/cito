//! Persistent, generic HumanChannel NDJSON subprocess transport.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{
    atomic::{AtomicU8, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};

use super::plugins::{load_plugins_from_roots, plugin_command, PluginDescriptor};

pub fn discover(
    roots: &[(PathBuf, bool)],
    user_root: Option<&Path>,
) -> Result<Vec<PluginDescriptor>, String> {
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
    // Filter before deduplicating across roots: a descriptor that cannot run on
    // the Hub must not suppress a lower-priority Hub plugin of the same ID.
    let mut seen = std::collections::HashSet::new();
    let mut plugins = Vec::new();
    for (root, builtin) in external
        .iter()
        .map(|r| (r, false))
        .chain(builtin.iter().map(|r| (r, true)))
    {
        let roots = std::slice::from_ref(root);
        let (external, bundled) = if builtin {
            (&[][..], roots)
        } else {
            (roots, &[][..])
        };
        for plugin in load_plugins_from_roots(external, bundled, user_root, true).plugins {
            if plugin.kind == "human-channel"
                && plugin.channel.hub_scoped
                && std::fs::read(plugin.plugin_dir.join("plugin.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    .is_some_and(|manifest| {
                        matches!(manifest["kind"].as_str(), Some("human" | "human-channel"))
                            && manifest.get("scope").is_none_or(|scope| scope == "hub")
                    })
                && seen.insert(plugin.id.clone())
            {
                plugins.push(plugin);
            }
        }
    }
    plugins.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(plugins)
}

#[cfg(test)]
#[path = "../../tests/hub/hub_plugin_cases.rs"]
mod tests;

#[cfg(test)]
pub(super) fn test_descriptor(
    dir: &Path,
    id: &str,
    command: Vec<String>,
    mut manifest: Value,
) -> PluginDescriptor {
    manifest["id"] = json!(id);
    manifest["kind"] = json!("human-channel");
    if manifest["displayName"].is_null() {
        manifest["displayName"] = json!(id);
    }
    manifest["command"] = if command.is_empty() {
        json!(["./unused"])
    } else {
        json!(command)
    };
    let mut descriptor = super::plugins::parse_manifest(&dir.join(id), false, &manifest).unwrap();
    descriptor.plugin_dir = dir.to_path_buf();
    descriptor
}

type Pending = Arc<Mutex<HashMap<String, mpsc::SyncSender<Result<Value, String>>>>>;
// Monotonic: either pipe can fail; only an intentional shutdown suppresses the exit wake.
const RUNNING: u8 = 0;
const EXITED: u8 = 1;
const CANCELLING: u8 = 2;
const STOPPING: u8 = 3;
pub(super) const CHANNEL_RETRY_DELAY: Duration = Duration::from_secs(5);
const MAX_STDOUT_LINE_BYTES: usize = 8 * 1024 * 1024;

fn read_stdout_line(reader: &mut impl BufRead, line: &mut Vec<u8>) -> io::Result<usize> {
    // A WouldBlock retains the prefix. Allow one sentinel byte to distinguish
    // an oversized frame without reading the rest of it.
    let remaining = MAX_STDOUT_LINE_BYTES + 1 - line.len();
    let result = reader.take(remaining as u64).read_until(b'\n', line);
    if line.len() > MAX_STDOUT_LINE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Plugin frame exceeds 8 MiB",
        ));
    }
    result
}

pub struct ChannelProcess {
    child: Mutex<Child>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    state: Arc<AtomicU8>,
    pending: Pending,
    reader: Mutex<Option<thread::JoinHandle<()>>>,
    shutdown_wake: Arc<crate::support::WakePipe>,
}
impl ChannelProcess {
    pub fn spawn(
        plugin: &PluginDescriptor,
        callback: Arc<dyn Fn(Value) -> Option<Value> + Send + Sync>,
    ) -> Result<Self, String> {
        let shutdown_wake = Arc::new(crate::support::WakePipe::new()?);
        let mut child = plugin_command(plugin)?
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
                let error = std::io::Error::last_os_error();
                let _ = crate::support::teardown_child_group(&mut child);
                return Err(error.to_string());
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
                let error = std::io::Error::last_os_error();
                let _ = crate::support::teardown_child_group(&mut child);
                return Err(error.to_string());
            }
        }
        let exited = crate::support::child_exit_fd(child.id()).inspect_err(|_| {
            let _ = crate::support::teardown_child_group(&mut child);
        })?;
        let reader_wake = shutdown_wake.clone();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let state = Arc::new(AtomicU8::new(RUNNING));
        let (si, p, reader_state) = (stdin.clone(), pending.clone(), state.clone());
        let started = Instant::now();
        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            while reader_state.load(Ordering::SeqCst) == RUNNING {
                match read_stdout_line(&mut reader, &mut line) {
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
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
                    Ok(_) => {}
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
                                let _ = tx.send(Ok(frame));
                            }
                        }
                    }
                } else if reader_state.load(Ordering::SeqCst) == RUNNING {
                    if let Some(reply) = callback(frame) {
                        if send_line(&si, &reply).is_err() {
                            break;
                        }
                    }
                }
            }
            let unexpected_exit = reader_state.fetch_max(EXITED, Ordering::SeqCst) < CANCELLING;
            if let Ok(mut si) = si.lock() {
                si.take();
            }
            if let Ok(mut p) = p.lock() {
                for (_, tx) in p.drain() {
                    let _ = tx.send(Err("PLUGIN_EXEC_FAILED: Plugin subprocess exited".into()));
                }
            }
            if unexpected_exit {
                // Preserve the supervisor's failed-process retry interval if the
                // replacement itself exits immediately. Long-lived exits wake it now.
                let retry = CHANNEL_RETRY_DELAY.saturating_sub(started.elapsed());
                callback(
                    json!({"op":"process_exit","error":"Plugin subprocess exited","retry_ms":retry.as_millis() as u64}),
                );
            }
        });
        Ok(Self {
            child: Mutex::new(child),
            stdin,
            state,
            pending,
            reader: Mutex::new(Some(reader)),
            shutdown_wake,
        })
    }
    pub fn alive(&self) -> bool {
        self.state.load(Ordering::SeqCst) == RUNNING
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
            self.state.fetch_max(EXITED, Ordering::SeqCst);
            self.shutdown_wake.notify();
            if let Ok(mut input) = self.stdin.lock() {
                input.take();
            }
        }
        result
    }
    pub fn request(&self, value: Value) -> Result<Value, String> {
        self.request_after_send(value, || {})
    }
    pub(super) fn request_after_send(
        &self,
        mut value: Value,
        after_send: impl FnOnce(),
    ) -> Result<Value, String> {
        let id = crate::support::try_random_hex(16)?;
        value["req_id"] = json!(id);
        let (tx, rx) = mpsc::sync_channel(1);
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| "Plugin pending lock poisoned")?;
            if self.state.load(Ordering::SeqCst) != RUNNING {
                return Err("PLUGIN_EXEC_FAILED: Plugin subprocess exited".into());
            }
            pending.insert(id.clone(), tx);
        }
        if let Err(e) = self.send(&value) {
            if let Ok(mut p) = self.pending.lock() {
                p.remove(&id);
            }
            return Err(e);
        }
        after_send();
        let result = rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| "PLUGIN_EXEC_FAILED: plugin request timed out".to_owned());
        if let Ok(mut p) = self.pending.lock() {
            p.remove(&id);
        }
        result?
    }
    pub(super) fn has_pending_requests(&self) -> bool {
        self.pending.lock().is_ok_and(|p| !p.is_empty())
    }
    pub(super) fn cancel_requests(&self) {
        self.state.fetch_max(CANCELLING, Ordering::SeqCst);
        if let Ok(mut pending) = self.pending.lock() {
            for (_, tx) in pending.drain() {
                let _ = tx.send(Err("UNAVAILABLE: Channel process is stopping".into()));
            }
        }
    }
    pub fn shutdown(&self) {
        let Ok(mut reader) = self.reader.lock() else {
            return;
        };
        if reader.is_none() {
            return;
        }
        let previous = self.state.swap(STOPPING, Ordering::SeqCst);
        self.cancel_requests();
        if matches!(previous, RUNNING | CANCELLING) {
            let _ = send_line(&self.stdin, &json!({"op":"shutdown"}));
        }
        // Closing stdin also triggers the protocol's plugin-side EOF lifecycle.
        if let Ok(mut stdin) = self.stdin.lock() {
            stdin.take();
        }
        self.shutdown_wake.notify();
        if let Ok(mut child) = self.child.lock() {
            // waitid also recognizes an exited child after its kqueue exit event
            // has passed. ECHILD prevents signaling after an external reap.
            let _ = crate::support::wait_child_exit(&child, Duration::from_millis(200));
            if super::plugins::peek_plugin_exit(child.id()).is_ok() {
                let _ = crate::support::teardown_child_group(&mut child);
            }
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
