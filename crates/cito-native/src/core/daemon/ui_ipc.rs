use super::{
    http_actions::{dispatch_action, interactive_login},
    wake_daemon, DaemonService,
};
use crate::core::identity::string;
use crate::setup::resolve_package_root;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

pub const FRAME_LIMIT: usize = 262_144;

pub(super) const REQUEST_LIMIT: usize = 1_048_576;

fn cap_text(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.into();
    }
    let mut end = limit.saturating_sub(3);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

fn bound_fields(v: &mut Value, key: &str, preserve_collections: bool) {
    match v {
        Value::String(s) => {
            if key == "request_id" {
                return;
            }
            if matches!(key, "title" | "from_title") {
                *s = cap_text(s, 128);
                return;
            }
            let cap = match key {
                "message" => 512,
                "about" | "from_about" => 256,
                "cwd" | "summary" => 128,
                _ => 4096,
            };
            *s = cap_text(s, cap);
        }
        Value::Array(a) => {
            let cap = match key {
                "activity" => 80,
                "peers" => 100,
                "sessions" | "listeners" | "my_remote_listeners" => 64,
                "available" => 32,
                _ => 128,
            };
            if !preserve_collections {
                a.truncate(cap);
            }
            for item in a {
                bound_fields(item, "", preserve_collections);
            }
        }
        Value::Object(m) => {
            for (k, v) in m {
                bound_fields(v, k, preserve_collections)
            }
        }
        _ => {}
    }
}

pub fn bounded_frame(mut frame: Value) -> Result<Vec<u8>, String> {
    // Full-message copy uses correlated bounded chunks rather than preview caps.
    if frame["op"] != "activity_message" {
        let correlated = frame.get("request_id").is_some_and(|id| !id.is_null());
        bound_fields(&mut frame, "", correlated);
    }
    loop {
        let bytes = serde_json::to_vec(&frame).map_err(|e| e.to_string())?;
        if bytes.len() + 1 < FRAME_LIMIT {
            return Ok(bytes);
        }
        // A correlated projection is a receipt: dropping rows can strand the
        // consumer's pending save. Send an explicit correlated failure instead.
        if frame.get("request_id").is_some_and(|id| !id.is_null()) {
            if frame["op"] == "error" {
                return Err("UI receipt exceeds frame budget".into());
            }
            frame = json!({"op":"error","ok":false,"request_id":frame["request_id"],"hub":frame["hub"],"channel":frame["channel"],"channel_id":frame["channel_id"],"action":frame["action"],"source_op":frame.get("source_op").unwrap_or(&frame["op"]),"error":"Response exceeds the UI frame budget; retry with a smaller selection"});
            continue;
        }
        let mut removed = false;
        for key in [
            "activity",
            "peers",
            "my_remote_listeners",
            "available",
            "sessions",
            "listeners",
            "harnesses",
            "hubs",
        ] {
            if let Some(list) = frame.get_mut(key).and_then(Value::as_array_mut) {
                if list.pop().is_some() {
                    removed = true;
                    break;
                }
            }
        }
        if !removed {
            if let Some(settings) = frame.get_mut("settings").and_then(Value::as_object_mut) {
                for key in ["apps", "channels", "hubs"] {
                    if let Some(list) = settings.get_mut(key).and_then(Value::as_array_mut) {
                        if list.pop().is_some() {
                            removed = true;
                            break;
                        }
                    }
                }
            }
        }
        if !removed {
            return Err("UI frame exceeds 256 KiB after collection shedding".into());
        }
        frame["truncated"] = json!(true);
    }
}

#[derive(Default)]
pub(super) struct UiMailbox {
    pub(super) pending: Mutex<std::collections::VecDeque<(Option<String>, Vec<u8>)>>,
    ready: std::sync::Condvar,
    pub(super) closed: AtomicBool,
}

impl UiMailbox {
    pub(super) fn emit(&self, frame: Value) {
        // Passive snapshots coalesce; correlated command results stay ordered.
        let key = if string(&frame, "request_id").is_empty()
            && string(&frame, "source_op").is_empty()
            && matches!(string(&frame, "op"), "state" | "settings" | "directory")
        {
            Some(format!(
                "{}:{}",
                string(&frame, "op"),
                string(&frame, "hub")
            ))
        } else {
            None
        };
        match bounded_frame(frame) {
            Ok(mut bytes) => {
                bytes.push(b'\n');
                self.push(key, bytes);
            }
            Err(error) => {
                eprintln!("cito-ui: {error}; closing the UI stream");
                self.close();
            }
        }
    }
    fn push(&self, key: Option<String>, bytes: Vec<u8>) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        if let Ok(mut pending) = self.pending.lock() {
            if let Some(key) = key.as_ref() {
                if let Some((_, frame)) = pending
                    .iter_mut()
                    .find(|(old, _)| old.as_ref() == Some(key))
                {
                    *frame = bytes;
                    self.ready.notify_one();
                    return;
                }
            }
            const MAX_PENDING: usize = 64;
            if pending.len() >= MAX_PENDING {
                if let Some(index) = pending.iter().position(|(key, _)| key.is_some()) {
                    pending.remove(index);
                } else {
                    // An unresponsive companion must reconnect rather than let
                    // correlated results grow without bound or disappear silently.
                    self.closed.store(true, Ordering::SeqCst);
                    self.ready.notify_all();
                    return;
                }
            }
            pending.push_back((key, bytes));
            self.ready.notify_one();
        }
    }
    pub(super) fn next(&self) -> Option<Vec<u8>> {
        let mut pending = self.pending.lock().ok()?;
        loop {
            if self.closed.load(Ordering::SeqCst) {
                return None;
            }
            if let Some((_, bytes)) = pending.pop_front() {
                return Some(bytes);
            }
            pending = self.ready.wait(pending).ok()?;
        }
    }
    pub(super) fn close(&self) {
        // The predicate and wait share this mutex so shutdown cannot lose its wake.
        let _pending = self.pending.lock();
        self.closed.store(true, Ordering::SeqCst);
        self.ready.notify_all();
    }
}

pub(super) struct UiChild {
    pub(super) child: Child,
    pub(super) reader: thread::JoinHandle<()>,
    pub(super) writer: thread::JoinHandle<()>,
}

pub(super) fn resolve_ui_binary(
    root: &std::path::Path,
    explicit: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    explicit
        .into_iter()
        .chain([
            root.join("Helpers/cito-ui"),
            root.join("bin/cito-ui-darwin-arm64"),
        ])
        .find(|path| path.is_file())
}

pub(super) fn spawn_ui(service: Arc<Mutex<DaemonService>>) -> Option<UiChild> {
    if std::env::var("CITO_DISABLE_UI").ok().as_deref() == Some("1") {
        return None;
    }
    let bin = resolve_ui_binary(
        &resolve_package_root(),
        std::env::var_os("CITO_UI_BINARY").map(std::path::PathBuf::from),
    )?;
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut input = child.stdin.take()?;
    let output = child.stdout.take()?;
    let mailbox = Arc::new(UiMailbox::default());
    let output_mailbox = mailbox.clone();
    let writer = thread::spawn(move || {
        while let Some(bytes) = output_mailbox.next() {
            if input.write_all(&bytes).and_then(|_| input.flush()).is_err() {
                break;
            }
        }
    });
    let ui_generation = if let Ok(mut s) = service.lock() {
        s.set_ui(mailbox.clone());
        s.ui_generation
    } else {
        mailbox.close();
        let _ = child.kill();
        let _ = child.wait();
        let _ = writer.join();
        return None;
    };
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(output);
        let mut logins: Vec<thread::JoinHandle<()>> = Vec::new();
        loop {
            let mut line = Vec::new();
            match reader
                .by_ref()
                .take((REQUEST_LIMIT + 1) as u64)
                .read_until(b'\n', &mut line)
            {
                Ok(0) | Err(_) => break,
                _ => {}
            }
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.len() > REQUEST_LIMIT {
                // Discard the rest without allocating or interpreting a partial UTF-8 action.
                let drained = reader.skip_until(b'\n');
                if let Ok(s) = service.lock() {
                    let error = "INVALID_INPUT: Message is too large to send";
                    s.emit(json!({"op":"error","error":error}));
                }
                if drained.is_err() {
                    break;
                }
                continue;
            }
            if let Ok(action) = serde_json::from_slice::<Value>(&line) {
                let is_login = interactive_login(&action);
                let service = service.clone();
                let run = move || {
                    // Both UI stdout actions and HTTP control callers share the
                    // same closed-loop error frame and refreshed state emission.
                    let _ = dispatch_action(&service, &action, Some(ui_generation));
                };
                if is_login {
                    logins.push(thread::spawn(run));
                } else {
                    run();
                }
                let mut pending = Vec::new();
                for login in logins.drain(..) {
                    if login.is_finished() {
                        let _ = login.join();
                    } else {
                        pending.push(login);
                    }
                }
                logins = pending;
            }
        }
        if let Ok(mut s) = service.lock() {
            // An old reader must never clear demand belonging to its replacement.
            if s.ui
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, &mailbox))
            {
                s.disconnect_ui();
            }
        }
        wake_daemon();
        for login in logins {
            let _ = login.join();
        }
    });
    Some(UiChild {
        child,
        reader,
        writer,
    })
}
