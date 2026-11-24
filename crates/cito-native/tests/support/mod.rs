use cito_native::{database::Database, support::wait_child_exit};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};

pub const NATIVE: &str = env!("CARGO_BIN_EXE_cito-native");
pub const PI: &str = env!("CARGO_BIN_EXE_cito-plugin-pi");
pub const CODEX: &str = env!("CARGO_BIN_EXE_cito-plugin-codex");
pub const CLAUDE: &str = env!("CARGO_BIN_EXE_cito-plugin-claude");
pub const HOOK: &str = env!("CARGO_BIN_EXE_cito-claude-hook");
pub const TELEGRAM: &str = env!("CARGO_BIN_EXE_cito-plugin-telegram");
pub const HUMAN: &str = "{\"expected_harness\":\"human\",\"expected_session_id\":\"default\"}";
static PROCESS_TEST: Mutex<()> = Mutex::new(());
static NEXT: AtomicU64 = AtomicU64::new(0);
// One process-test owner at a time, including tests that need multiple hubs.
// Cargo runs integration targets sequentially; unit engine tests stay in-memory.
pub fn serial() -> MutexGuard<'static, ()> {
    PROCESS_TEST.lock().unwrap_or_else(|p| p.into_inner())
}
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
pub fn port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
#[track_caller]
pub fn wait(predicate: impl FnMut() -> bool) {
    wait_for(Duration::from_secs(12), predicate);
}
#[track_caller]
fn wait_for(timeout: Duration, mut predicate: impl FnMut() -> bool) {
    let until = Instant::now() + timeout;
    loop {
        if predicate() {
            return;
        }
        assert!(Instant::now() < until, "condition timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[track_caller]
pub fn read_json(path: impl AsRef<Path>) -> Value {
    let path = path.as_ref();
    let contents = fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_slice(&contents).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}
pub fn script(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
pub fn http(port: u16, path: &str, body: Option<&Value>) -> (u16, Value) {
    try_http(port, path, body).unwrap_or_else(|error| panic!("{error}"))
}

pub fn try_http(port: u16, path: &str, body: Option<&Value>) -> Result<(u16, Value), String> {
    let url = format!("http://127.0.0.1:{port}{path}");
    let a = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build();
    let result = match body {
        Some(v) => a.post(&url).send_json(v),
        None => a.get(&url).call(),
    };
    let response = match result {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(format!("{url}: {e}")),
    };
    let status = response.status();
    let text = response
        .into_string()
        .map_err(|error| format!("{url}: {error}"))?;
    let value = serde_json::from_str(&text)
        .or_else(|_| {
            serde_json::from_str(
                text.lines()
                    .find_map(|l| l.strip_prefix("data:"))
                    .unwrap_or(""),
            )
        })
        .map_err(|_| format!("{url}: {text}"))?;
    Ok((status, value))
}
// Keep endpoint PIDs stable while giving their stdin a real terminal vnode.
pub fn spawn_interactive(f: &Fixture, bin: &str, args: &[&str], extra: &[(&str, &str)]) -> usize {
    let mut command = vec!["-c", "import os,sys; m,s=os.openpty(); os.dup2(s,0); os.set_inheritable(m,True); os.close(s); os.execvp(sys.argv[1],sys.argv[1:])", bin];
    command.extend_from_slice(args);
    f.spawn("python3", &command, extra)
}

pub fn ui_frames(f: &Fixture) -> Vec<Value> {
    fs::read_to_string(f.home.join("ui-frames"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}
pub fn install_builtin_override(f: &Fixture, id: &str, manifest: Value) -> PathBuf {
    let path = f.state.join("plugins").join(id);
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("plugin.json"), manifest.to_string()).unwrap();
    path
}
pub fn install_provider_command(f: &Fixture, id: &str, command: Value) -> PathBuf {
    let mut manifest: Value = serde_json::from_slice(
        &fs::read(root().join("plugins").join(id).join("plugin.json")).unwrap(),
    )
    .unwrap();
    manifest["command"] = command;
    install_builtin_override(f, id, manifest)
}
pub fn channel_provider(f: &Fixture, id: &str, fields: Value) -> PathBuf {
    install_builtin_override(
        f,
        id,
        json!({"id":id,"kind":"human-channel","scope":"hub","displayName":"Fixture Channel",
        "command":["python3",root().join("crates/cito-native/tests/hub/channel.py")],"fields":fields}),
    )
}

struct Process {
    child: Child,
    stopped: Option<Result<ExitStatus, String>>,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    readers: Vec<std::thread::JoinHandle<()>>,
}
impl Process {
    fn stop(&mut self) -> Result<ExitStatus, String> {
        if let Some(result) = &self.stopped {
            return result.clone();
        }
        let mut errors = Vec::new();
        let signal_group = |signal| {
            // SAFETY: process_group(0) gives the child its own group. Only wait() below
            // releases its PID, so even an already-exited leader still reserves the ID.
            if unsafe { libc::kill(-(self.child.id() as i32), signal) } < 0 {
                let error = std::io::Error::last_os_error();
                match error.raw_os_error() {
                    Some(libc::ESRCH) => {}
                    // macOS can return EPERM when only the zombie leader remains.
                    Some(libc::EPERM)
                        if wait_child_exit(&self.child, Duration::ZERO) == Ok(true) => {}
                    _ => return Err(format!("Cannot signal child group: {error}")),
                }
            }
            Ok(())
        };
        let graceful = wait_child_exit(&self.child, Duration::ZERO).and_then(|exited| {
            if !exited {
                signal_group(libc::SIGTERM)?;
                wait_child_exit(&self.child, Duration::from_secs(2))?;
            }
            Ok(())
        });
        errors.extend(graceful.err());
        errors.extend(signal_group(libc::SIGKILL).err());
        let status = self
            .child
            .wait()
            .map_err(|e| format!("Cannot reap child: {e}"));

        let until = Instant::now() + Duration::from_secs(2);
        for reader in self.readers.drain(..) {
            while !reader.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            if !reader.is_finished() {
                errors.push("Process output reader timed out".into());
            } else if reader.join().is_err() {
                errors.push("Process output reader panicked".into());
            }
        }
        let result = if errors.is_empty() {
            status
        } else {
            errors.extend(status.err());
            Err(errors.join("; "))
        };
        self.stopped = Some(result.clone());
        result
    }
}
pub struct Fixture {
    pub dir: PathBuf,
    pub state: PathBuf,
    pub home: PathBuf,
    pub config: PathBuf,
    pub port: u16,
    pub env: BTreeMap<String, String>,
    children: RefCell<Vec<Process>>,
}
impl Fixture {
    pub fn new(config: Value) -> Self {
        let dir = PathBuf::from(format!(
            "/tmp/ct-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let home = dir.join("home");
        let state = dir.join("state");
        let path = state.join("config.json");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(&path, config.to_string()).unwrap();
        let mock = dir.join("mock");
        script(
            &mock,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$HOME/mock-calls\"\n",
        );
        let port = port();
        let mut env = BTreeMap::new();
        for (k, v) in [
            ("HOME", home.display().to_string()),
            ("CITO_STATE_DIR", state.display().to_string()),
            ("CITO_CONFIG_PATH", path.display().to_string()),
            ("CITO_DAEMON_PORT", port.to_string()),
            ("PATH", std::env::var("PATH").unwrap()),
            ("TMPDIR", dir.display().to_string()),
            ("USER", "alice".into()),
            ("CITO_USER", "alice".into()),
            ("CITO_HOST_NAME", "mac-studio".into()),
            ("CITO_PI_PROCESS_DISCOVERY", "0".into()),
            ("CITO_PI_ALLOW_HEADLESS_DISCOVERY", "1".into()),
            ("CITO_DISABLE_UI", "1".into()),
            ("CITO_UI_HEADLESS", "1".into()),
            (
                "CITO_BUILTIN_PLUGINS_DIR",
                root().join("plugins").display().to_string(),
            ),
        ] {
            env.insert(k.into(), v);
        }
        for key in ["CITO_OPEN_BIN", "CITO_BROWSER_BIN"] {
            env.insert(key.into(), mock.display().to_string());
        }
        Self {
            dir,
            state,
            home,
            config: path,
            port,
            env,
            children: RefCell::new(Vec::new()),
        }
    }
    pub fn spawn(&self, bin: &str, args: &[&str], extra: &[(&str, &str)]) -> usize {
        let mut command = self.command_builder(bin, args);
        command.envs(extra.iter().copied()).stdin(Stdio::piped());
        let mut child = command.spawn().unwrap_or_else(|e| panic!("{bin}: {e}"));
        let stdout = Arc::new(Mutex::new(String::new()));
        let stderr = Arc::new(Mutex::new(String::new()));
        let mut readers = Vec::new();
        for (input, output) in [
            (
                Box::new(child.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>,
                stdout.clone(),
            ),
            (
                Box::new(child.stderr.take().unwrap()) as Box<dyn std::io::Read + Send>,
                stderr.clone(),
            ),
        ] {
            readers.push(std::thread::spawn(move || {
                for line in BufReader::new(input).lines().map_while(Result::ok) {
                    let mut text = output.lock().unwrap();
                    text.push_str(&line);
                    text.push('\n');
                }
            }));
        }
        let mut children = self.children.borrow_mut();
        let index = children.len();
        children.push(Process {
            child,
            stopped: None,
            stdout,
            stderr,
            readers,
        });
        index
    }
    pub fn command_builder(&self, bin: &str, args: &[&str]) -> Command {
        let mut command = Command::new(bin);
        command
            .args(args)
            .env_clear()
            .envs(&self.env)
            .current_dir(&self.dir)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
    pub fn send_raw(&self, index: usize, bytes: &[u8]) {
        let mut children = self.children.borrow_mut();
        let input = children[index].child.stdin.as_mut().unwrap();
        input.write_all(bytes).unwrap();
        input.flush().unwrap();
    }
    pub fn send(&self, index: usize, value: Value) {
        self.send_raw(index, format!("{value}\n").as_bytes());
    }
    pub fn output(&self, index: usize, error: bool) -> String {
        let children = self.children.borrow();
        let p = &children[index];
        let value = if error { &p.stderr } else { &p.stdout };
        let text = value.lock().unwrap().clone();
        text
    }
    pub fn frames(&self, index: usize) -> Vec<Value> {
        self.output(index, false)
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
    pub fn frames_for<'a>(
        &self,
        index: usize,
        op: &'a str,
    ) -> impl DoubleEndedIterator<Item = Value> + 'a {
        self.frames(index)
            .into_iter()
            .filter(move |frame| frame["op"] == op)
    }
    pub fn eof(&self, index: usize) {
        self.children.borrow_mut()[index].child.stdin.take();
    }
    pub fn pid(&self, index: usize) -> u32 {
        self.children.borrow()[index].child.id()
    }
    pub fn signal(&self, index: usize, signal: i32) {
        let children = self.children.borrow();
        let p = &children[index];
        if p.stopped.is_none() {
            // SAFETY: the fixture still owns this unreaped PID.
            unsafe { libc::kill(p.child.id() as i32, signal) };
        }
    }
    pub fn exited(&self, index: usize) -> bool {
        let children = self.children.borrow();
        let p = &children[index];
        match &p.stopped {
            Some(Ok(_)) => true,
            Some(Err(error)) => panic!("Process cleanup failed: {error}"),
            None => wait_child_exit(&p.child, Duration::ZERO).unwrap(),
        }
    }
    pub fn stop(&self, index: usize) {
        self.children.borrow_mut()[index]
            .stop()
            .unwrap_or_else(|error| panic!("Process cleanup failed: {error}"));
    }
    pub fn command(&self, bin: &str, args: &[&str], input: &str) -> (bool, String, String) {
        let i = self.spawn(bin, args, &[]);
        if !input.is_empty() {
            self.send_raw(i, input.as_bytes());
        }
        self.eof(i);
        wait_for(Duration::from_secs(30), || self.exited(i));
        let success = self.children.borrow_mut()[i]
            .stop()
            .unwrap_or_else(|error| panic!("Process cleanup failed: {error}"))
            .success();
        (success, self.output(i, false), self.output(i, true))
    }
    pub fn plugin(&self, bin: &str, mut request: Value) -> Value {
        request["state_dir"] = json!(self.state);
        request["env"] = json!(self.env);
        let (ok, out, err) = self.command(bin, &[], &format!("{request}\n"));
        assert!(ok, "{err}");
        serde_json::from_str(&out).unwrap_or_else(|_| panic!("{out}: {err}"))
    }
    pub fn daemon(&self, extra: &[(&str, &str)]) -> usize {
        let index = self.spawn(NATIVE, &["daemon"], extra);
        wait(|| {
            assert!(!self.exited(index), "{}", self.output(index, true));
            if TcpStream::connect(("127.0.0.1", self.port)).is_err() {
                return false;
            }
            let (s, h) = http(self.port, "/health", None);
            s == 200
                && h["version"] == env!("CARGO_PKG_VERSION")
                && h["stateDir"] == json!(self.state)
        });
        index
    }
    pub fn hub(&self) -> (usize, u16) {
        let p = port();
        let index = self.spawn(
            NATIVE,
            &[
                "hub",
                "--bind",
                "127.0.0.1",
                "--port",
                &p.to_string(),
                "--data-dir",
                self.state.join("hub").to_str().unwrap(),
                "--auth",
                "none",
            ],
            &[],
        );
        wait(|| TcpStream::connect(("127.0.0.1", p)).is_ok());
        (index, p)
    }
    pub fn action(&self, value: Value) -> (u16, Value) {
        http(self.port, "/ui", Some(&value))
    }
    #[track_caller]
    pub fn action_ok(&self, action: Value) -> Value {
        let (status, body) = self.action(action);
        assert_eq!(status, 200, "{body}");
        body
    }
    pub fn snapshot(&self) -> Value {
        self.action_ok(json!({"op":"get_snapshot"}))
    }
    pub fn mcp(&self, method: &str, params: Value) -> Value {
        let (s, v) = http(
            self.port,
            "/mcp",
            Some(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})),
        );
        assert_eq!(s, 200, "{v}");
        assert!(v.get("error").is_none(), "{v}");
        v["result"].clone()
    }
    pub fn tool_result(&self, name: &str, args: Value, meta: Value) -> Value {
        self.mcp(
            "tools/call",
            json!({"name":name,"arguments":args,"_meta":meta}),
        )
    }
    pub fn tool(&self, name: &str, args: Value, meta: Value) -> Value {
        let v = self.tool_result(name, args, meta);
        assert_ne!(v["isError"], true, "{v}");
        serde_json::from_str(v["content"][0]["text"].as_str().unwrap()).unwrap()
    }
    pub fn db(&self, name: &str) -> Database {
        Database::open(&self.state.join(name)).unwrap()
    }
    pub fn rows(&self, sql: &str) -> Vec<Value> {
        self.db("state.sqlite").query(sql, &[]).unwrap()
    }
    pub fn recording_daemon(&self) -> usize {
        self.daemon(&[
            ("CITO_DISABLE_UI", "0"),
            ("CITO_UI_BINARY", self.recording_ui().to_str().unwrap()),
        ])
    }
    pub fn recording_ui(&self) -> PathBuf {
        let path = self.dir.join("ui.py");
        script(&path, "#!/usr/bin/env python3\nimport os,sys\nopen(os.environ['HOME']+'/ui-pid','w').write(str(os.getpid()))\nwith open(os.environ['HOME']+'/ui-frames','a') as out:\n for line in sys.stdin:\n  out.write(line); out.flush()\n");
        path
    }
    pub fn provider(&self, outcome: &str) -> Value {
        let d = install_builtin_override(
            self,
            "fixture",
            json!({"id":"fixture","kind":"agent-harness","displayName":"Fixture","tier":"experimental","command":["python3","./driver.py"]}),
        );
        fs::write(
            d.join("options.json"),
            json!({"session_id":"session-1","status":"alive","outcome":outcome}).to_string(),
        )
        .unwrap();
        fs::write(d.join("driver.py"), include_str!("provider.py")).unwrap();
        json!({"expected_harness":"fixture","expected_session_id":"session-1"})
    }
}
#[test]
#[ignore = "subprocess worker for teardown regression"]
fn fixture_teardown_failure_worker() {
    let mode = std::env::var("CITO_TEARDOWN_CASE").unwrap();
    let fixture = Fixture::new(json!({}));
    fs::write(
        std::env::var("CITO_TEARDOWN_PATH").unwrap(),
        fixture.dir.to_str().unwrap(),
    )
    .unwrap();
    if mode == "command-fast-exit" {
        assert!(fixture.command("/bin/sh", &["-c", "exit 0"], "").0);
        return;
    }
    if mode == "drop-fast-exit" {
        let i = fixture.spawn("/bin/sh", &["-c", "exit 0"], &[]);
        wait(|| fixture.exited(i));
        return;
    }
    if mode.ends_with("-descendant") {
        // The sleep inherits both output pipes. Its PID is recorded outside the
        // worker's sandbox so the caller can check liveness even after a panic.
        let pid_file = std::env::var("CITO_TEARDOWN_PID").unwrap();
        let args = [
            "-c",
            "sleep 30 & echo $! > \"$1\"; echo out; echo err >&2; exit 23",
            "sh",
            &pid_file,
        ];
        if mode == "command-descendant" {
            assert_eq!(
                fixture.command("/bin/sh", &args, ""),
                (false, "out\n".into(), "err\n".into())
            );
            assert!(fixture.exited(0));
            fixture.stop(0);
            return;
        }
        let i = fixture.spawn("/bin/sh", &args, &[]);
        wait(|| fixture.exited(i));
        if mode == "drop-descendant" {
            return;
        }
        // Continue into the failing health probe during primary panic cleanup.
    }
    if matches!(mode.as_str(), "ordinary-reader" | "ordinary-reader-timeout") {
        let i = fixture.spawn("/bin/sh", &["-c", "exit 0"], &[]);
        fixture.children.borrow_mut()[i]
            .readers
            .push(std::thread::spawn(move || {
                if mode == "ordinary-reader" {
                    panic!("fixture reader failed");
                }
                loop {
                    std::thread::park();
                }
            }));
        return;
    }
    let listener = TcpListener::bind(("127.0.0.1", fixture.port)).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            if mode.contains("malformed") {
                let mut reader = BufReader::new(&mut stream);
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if line == "\r\n" => {
                            let _ = reader.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\ninvalid");
                            break;
                        }
                        Ok(_) => {}
                    }
                }
            }
            // Closing immediately also covers a successful TCP preflight followed by EOF.
        }
    });
    if std::env::var("CITO_TEARDOWN_CASE")
        .unwrap()
        .starts_with("panic")
    {
        panic!("primary assertion failure");
    }
    drop(fixture);
}

#[test]
fn fixture_teardown_settles_children_and_preserves_primary_failures() {
    use std::os::unix::process::ExitStatusExt;
    let _lock = serial();
    let parent = Fixture::new(json!({}));
    let marker = parent.dir.join("child-directory");
    for mode in [
        "panic-closed",
        "panic-malformed",
        "ordinary-closed",
        "ordinary-malformed",
        "ordinary-reader",
        "ordinary-reader-timeout",
        "command-fast-exit",
        "drop-fast-exit",
        "command-descendant",
        "drop-descendant",
        "panic-descendant",
    ] {
        let pid_file = parent.dir.join(format!("{mode}.pid"));
        let mut command = parent.command_builder(
            std::env::current_exe().unwrap().to_str().unwrap(),
            &[
                "--exact",
                "support::fixture_teardown_failure_worker",
                "--ignored",
                "--nocapture",
            ],
        );
        command
            .env("CITO_TEARDOWN_CASE", mode)
            .env("CITO_TEARDOWN_PATH", &marker)
            .env("CITO_TEARDOWN_PID", &pid_file);
        let output =
            cito_native::support::run_command_with_timeout(command, Duration::from_secs(8))
                .unwrap()
                .unwrap_or_else(|| panic!("{mode}: teardown timed out"));
        assert_eq!(output.status.signal(), None, "{mode}: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        if mode.starts_with("command-") || mode.starts_with("drop-") {
            assert!(output.status.success(), "{mode}: {output:?}");
        } else {
            assert_eq!(output.status.code(), Some(101), "{mode}: {output:?}");
            assert!(
                stderr.contains(if mode.starts_with("panic") {
                    "primary assertion failure"
                } else {
                    "Fixture teardown failed"
                }),
                "{mode}: {stderr}"
            );
        }
        if mode.ends_with("-descendant") {
            let pid = fs::read_to_string(pid_file)
                .unwrap()
                .trim()
                .parse::<u32>()
                .unwrap();
            assert!(pid > 1);
            wait(|| !cito_native::support::alive(pid));
        }
        if mode == "ordinary-reader-timeout" {
            assert!(
                stderr.contains("Process output reader timed out"),
                "{stderr}"
            );
        }
        if mode == "ordinary-reader" {
            assert!(
                stderr.contains("Process output reader panicked"),
                "{stderr}"
            );
        }
        let child_dir = fs::read_to_string(&marker).unwrap();
        fs::remove_file(&marker).unwrap();
        assert!(
            !Path::new(&child_dir).exists(),
            "Fixture directory survived: {child_dir}"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let mut failures = Vec::new();
        for p in self.children.get_mut().iter_mut().rev() {
            failures.extend(p.stop().err());
        }
        // CLI auto-starts a detached daemon. Only stop a health-verified sandbox owner.
        if TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
            let health = try_http(self.port, "/health", None);
            if let Err(error) = &health {
                failures.push(error.clone());
            }
            let h = health.map(|(_, value)| value).unwrap_or(Value::Null);
            if h["stateDir"] == json!(self.state) {
                if let Some(pid) = h["pid"].as_i64().filter(|p| *p > 1) {
                    unsafe {
                        libc::kill(pid as i32, libc::SIGTERM);
                    }
                    let until = Instant::now() + Duration::from_secs(3);
                    while TcpStream::connect(("127.0.0.1", self.port)).is_ok()
                        && Instant::now() < until
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    if TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                        failures.push("detached daemon did not stop".into());
                    }
                }
            }
        }
        let removed = fs::remove_dir_all(&self.dir);
        assert!(
            std::thread::panicking() || (removed.is_ok() && failures.is_empty()),
            "Fixture teardown failed: errors={failures:?}, removal={removed:?}"
        );
    }
}
