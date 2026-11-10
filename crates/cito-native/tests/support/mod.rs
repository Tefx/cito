use cito_native::database::Database;
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
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
pub fn wait(mut predicate: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(12);
    loop {
        if predicate() {
            return;
        }
        assert!(Instant::now() < until, "condition timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
pub fn script(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
pub fn http(port: u16, path: &str, body: Option<&Value>) -> (u16, Value) {
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
        Err(e) => panic!("{url}: {e}"),
    };
    let status = response.status();
    let text = response.into_string().unwrap();
    let value = serde_json::from_str(&text)
        .or_else(|_| {
            serde_json::from_str(
                text.lines()
                    .find_map(|l| l.strip_prefix("data:"))
                    .unwrap_or(""),
            )
        })
        .unwrap_or_else(|_| panic!("{url}: {text}"));
    (status, value)
}
struct Process {
    child: Child,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    readers: Vec<std::thread::JoinHandle<()>>,
}
impl Process {
    fn stop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            // SAFETY: each owned child leads its own process group. Reap before its ID can be reused.
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGTERM);
            }
            let until = Instant::now() + Duration::from_secs(2);
            while self.child.try_wait().unwrap().is_none() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            if self.child.try_wait().unwrap().is_none() {
                unsafe {
                    libc::kill(-(self.child.id() as i32), libc::SIGKILL);
                }
            }
        }
        self.child.wait().unwrap();
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
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
            ("CITO_PI_PROCESS_DISCOVERY", "0".into()),
            ("CITO_DISABLE_UI", "1".into()),
            ("CITO_UI_HEADLESS", "1".into()),
            ("CITO_BANNER_HEADLESS", "1".into()),
            (
                "CITO_BUILTIN_PLUGINS_DIR",
                root().join("plugins").display().to_string(),
            ),
        ] {
            env.insert(k.into(), v);
        }
        for key in ["CITO_OPEN_BIN", "CITO_BROWSER_BIN", "CITO_LAUNCHCTL_BIN"] {
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
        let mut command = Command::new(bin);
        command
            .args(args)
            .env_clear()
            .envs(&self.env)
            .envs(extra.iter().copied())
            .current_dir(&self.dir)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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
            stdout,
            stderr,
            readers,
        });
        index
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
    pub fn eof(&self, index: usize) {
        self.children.borrow_mut()[index].child.stdin.take();
    }
    pub fn pid(&self, index: usize) -> u32 {
        self.children.borrow()[index].child.id()
    }
    pub fn signal(&self, index: usize, signal: i32) {
        unsafe {
            libc::kill(self.pid(index) as i32, signal);
        }
    }
    pub fn exited(&self, index: usize) -> bool {
        self.children.borrow_mut()[index]
            .child
            .try_wait()
            .unwrap()
            .is_some()
    }
    pub fn stop(&self, index: usize) {
        self.children.borrow_mut()[index].stop();
    }
    pub fn command(&self, bin: &str, args: &[&str], input: &str) -> (bool, String, String) {
        let i = self.spawn(bin, args, &[]);
        if !input.is_empty() {
            self.send_raw(i, input.as_bytes());
        }
        self.eof(i);
        wait(|| self.exited(i));
        self.stop(i);
        let success = self.children.borrow_mut()[i]
            .child
            .wait()
            .unwrap()
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
            s == 200 && h["version"] == "0.20.2" && h["stateDir"] == json!(self.state)
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
    pub fn recording_ui(&self) -> PathBuf {
        let path = self.dir.join("ui.py");
        script(&path, "#!/usr/bin/env python3\nimport os,sys\nopen(os.environ['HOME']+'/ui-pid','w').write(str(os.getpid()))\nwith open(os.environ['HOME']+'/ui-frames','a') as out:\n for line in sys.stdin:\n  out.write(line); out.flush()\n");
        path
    }
    pub fn provider(&self, outcome: &str) -> Value {
        let d = self.state.join("plugins/fixture");
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("options.json"),
            json!({"session_id":"session-1","status":"alive","outcome":outcome}).to_string(),
        )
        .unwrap();
        fs::write(d.join("driver.py"), include_str!("provider.py")).unwrap();
        fs::write(d.join("plugin.json"),json!({"id":"fixture","kind":"agent-harness","displayName":"Fixture","tier":"experimental","command":["python3","./driver.py"]}).to_string()).unwrap();
        json!({"expected_harness":"fixture","expected_session_id":"session-1"})
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for p in self.children.get_mut().iter_mut().rev() {
            p.stop();
        }
        // CLI auto-starts a detached daemon. Only stop a health-verified sandbox owner.
        if TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
            let (_, h) = http(self.port, "/health", None);
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
                    assert!(
                        TcpStream::connect(("127.0.0.1", self.port)).is_err(),
                        "detached daemon did not stop"
                    );
                }
            }
        }
        fs::remove_dir_all(&self.dir).unwrap();
    }
}
