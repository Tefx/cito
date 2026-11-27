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

pub(super) const REQUEST_LIMIT: usize = 1_048_576;

#[derive(Default)]
pub(super) struct UiMailbox {
    pub(super) pending: Mutex<std::collections::VecDeque<(Option<String>, Vec<u8>)>>,
    ready: std::sync::Condvar,
    pub(super) closed: AtomicBool,
}

impl UiMailbox {
    pub(super) fn emit(&self, frame: Value) -> bool {
        // Passive snapshots coalesce; correlated command results stay ordered.
        let key = if frame["request_id"].is_null()
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
        let mut bytes = match serde_json::to_vec(&frame) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("cito-ui: Cannot encode frame: {error}");
                self.close();
                wake_daemon();
                return false;
            }
        };
        bytes.push(b'\n');
        let Ok(mut pending) = self.pending.lock() else {
            return false;
        };
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        if let Some(key) = key.as_ref() {
            pending.retain(|(old, _)| old.as_ref() != Some(key));
        }
        // The pipe writer drains independently of the daemon lock. Only superseded
        // snapshots are replaceable; events and receipts have no overflow eviction.
        pending.push_back((key, bytes));
        self.ready.notify_one();
        true
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
    pub(super) mailbox: Arc<UiMailbox>,
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
                output_mailbox.close();
                wake_daemon();
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
    let child_mailbox = mailbox.clone();
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
            let complete = line.last() == Some(&b'\n');
            if complete {
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
            if !complete {
                break;
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
        mailbox: child_mailbox,
        child,
        reader,
        writer,
    })
}
