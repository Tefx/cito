#![allow(dead_code)]
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub type Result<T> = std::result::Result<T, String>;
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}
pub fn env_value(req: &Value, key: &str) -> Option<String> {
    req.get("env")
        .and_then(|v| v.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| std::env::var(key).ok())
}
pub fn home(req: &Value) -> PathBuf {
    PathBuf::from(env_value(req, "HOME").unwrap_or_else(|| "/tmp".into()))
}
pub fn expand(req: &Value, value: &str) -> PathBuf {
    if value == "~" {
        home(req)
    } else if let Some(s) = value.strip_prefix("~/") {
        home(req).join(s)
    } else {
        let p = PathBuf::from(value);
        if p.is_absolute() {
            p
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("/"))
                .join(p)
        }
    }
}
pub fn state(req: &Value) -> PathBuf {
    str_at(req, "state_dir")
        .map(|s| expand(req, s))
        .or_else(|| env_value(req, "CITO_STATE_DIR").map(|s| expand(req, &s)))
        .unwrap_or_else(|| home(req).join(".cito"))
}
pub fn config_home(req: &Value, env: &str, default: &str) -> PathBuf {
    expand(req, &env_value(req, env).unwrap_or_else(|| default.into()))
}
pub fn validate_session(s: &str) -> Result<String> {
    let s = s.trim();
    if s.is_empty()
        || s.len() > 128
        || !s.as_bytes()[0].is_ascii_alphanumeric()
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err("Invalid session identifier".into());
    }
    Ok(s.into())
}
pub fn error(code: &str, message: impl ToString) -> Value {
    json!({"ok":false,"error_code":code,"message":message.to_string()})
}
pub fn emit(v: &Value) {
    let out = std::io::stdout();
    let mut lock = out.lock();
    let _ = writeln!(lock, "{v}");
    let _ = lock.flush();
}
pub fn one_shot(handler: fn(&Value) -> Result<Value>) {
    let mut line = String::new();
    let result = std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())
        .and_then(|_| serde_json::from_str(&line).map_err(|e| e.to_string()))
        .and_then(|r| handler(&r));
    emit(&result.unwrap_or_else(|e| error("PLUGIN_EXEC_FAILED", e)));
}

// Unlink only while holding the lock. Waiters retry if the path no longer names
// their locked inode, so cleanup cannot split concurrent read/modify/write owners.
pub struct FileLock(File);
impl FileLock {
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        loop {
            let f = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(path)
                .map_err(|e| e.to_string())?;
            // SAFETY: flock borrows this live descriptor; Drop releases it.
            if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let lock = Self(f);
            let held = lock.0.metadata().map_err(|e| e.to_string())?;
            if fs::metadata(path).is_ok_and(|m| m.dev() == held.dev() && m.ino() == held.ino()) {
                return Ok(lock);
            }
        }
    }
}
use std::os::unix::fs::OpenOptionsExt;
impl Drop for FileLock {
    fn drop(&mut self) {
        // SAFETY: the owned file remains live during unlock.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
pub fn clean_sidecars(path: &Path) {
    let Some(parent) = path.parent() else { return };
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return;
    };
    let bak_prefix = format!("{name}.bak.");
    let tmp_prefix = format!("{name}.tmp-");
    if let Ok(entries) = fs::read_dir(parent) {
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().into_owned();
            if fname.starts_with(&bak_prefix) || fname.starts_with(&tmp_prefix) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    let mut tmp_name = path.as_os_str().to_os_string();
    tmp_name.push(format!(".tmp-{}-{}", std::process::id(), now_ms()));
    let tmp = PathBuf::from(tmp_name);
    let permissions = fs::metadata(path).ok().map(|m| m.permissions());
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    let r = (|| {
        f.write_all(bytes).map_err(|e| e.to_string())?;
        if let Some(permissions) = permissions {
            f.set_permissions(permissions).map_err(|e| e.to_string())?;
        }
        f.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&tmp, path).map_err(|e| e.to_string())
    })();
    if r.is_err() {
        let _ = fs::remove_file(tmp);
    }
    r
}
pub fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
pub fn read_json(path: &Path) -> Result<Value> {
    let Some(s) = read_optional(path)? else {
        return Ok(json!({}));
    };
    let v: Value =
        serde_json::from_str(&s).map_err(|e| format!("Invalid JSON in {}: {e}", path.display()))?;
    if !v.is_object() {
        return Err(format!("Expected JSON object in {}", path.display()));
    }
    Ok(v)
}
fn mutation_path(path: &Path, remaining: u8) -> Result<PathBuf> {
    if remaining == 0 {
        return Err("Too many symbolic links in configuration path".into());
    }
    if let Ok(canonical) = fs::canonicalize(path) {
        return Ok(canonical);
    }
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        let target = fs::read_link(path).map_err(|e| e.to_string())?;
        return mutation_path(
            &path.parent().unwrap_or(Path::new(".")).join(target),
            remaining - 1,
        );
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    match (parent, path.file_name()) {
        (Some(parent), Some(name)) => Ok(mutation_path(parent, remaining)?.join(name)),
        _ => Ok(path.to_owned()),
    }
}
const MCP_CONFLICT: &str = "Conflicting MCP command; pass --force to replace";
pub fn check_mcp_command(req: &Value, command: Option<&str>, expected: &str) -> Result<()> {
    if req["op"] != "setup_remove"
        && req["force"] != true
        && command.is_some_and(|c| c != expected && c != "cito-mcp" && !c.ends_with("/cito-mcp"))
    {
        return Err(MCP_CONFLICT.into());
    }
    Ok(())
}
pub fn mutate(
    req: &Value,
    id: &str,
    path: &Path,
    transform: impl FnOnce(Option<&str>) -> Result<Option<String>>,
) -> Result<Value> {
    let dry = req["dry_run"] == true;
    let remove = req["op"] == "setup_remove";
    let canonical = mutation_path(path, 40)?;
    let path = canonical.as_path();
    let lock_path = {
        let mut s = path.as_os_str().to_os_string();
        s.push(".cito.lock");
        PathBuf::from(s)
    };
    let old = {
        let _lock = if dry {
            None
        } else {
            Some(FileLock::acquire(&lock_path)?)
        };
        let old = read_optional(path)?;
        let next = match transform(old.as_deref()) {
            Err(error) if error == MCP_CONFLICT => {
                return Ok(
                    json!({"target":id,"path":path,"status":"conflict","backupPath":null,"detail":error}),
                )
            }
            result => result?,
        };
        let changed = next.as_deref() != old.as_deref();
        let status = if !changed {
            "unchanged"
        } else if old.is_none() && next.is_some() {
            "created"
        } else {
            "updated"
        };
        let mut backup = None;
        if changed && !dry {
            if !remove {
                if let Some(ref content) = old {
                    let mut backup_name = path.as_os_str().to_os_string();
                    backup_name.push(format!(".bak.{}-{}", now_ms(), std::process::id()));
                    let p = PathBuf::from(backup_name);
                    fs::write(&p, content).map_err(|e| e.to_string())?;
                    fs::set_permissions(&p, fs::Permissions::from_mode(0o600))
                        .map_err(|e| e.to_string())?;
                    backup = Some(p);
                }
            }
            match next {
                Some(ref s) => atomic_write(path, s.as_bytes())?,
                None => {
                    if path.exists() {
                        fs::remove_file(path).map_err(|e| e.to_string())?
                    }
                }
            }
        }
        if !dry && (remove || old.is_none() && !changed) {
            if remove {
                clean_sidecars(path);
            }
            let _ = fs::remove_file(&lock_path);
        }
        (status, backup)
    };
    Ok(
        json!({"target":id,"path":path,"status":old.0,"backupPath":old.1,"detail":if dry{"Planned configuration update"}else{"Configuration checked"}}),
    )
}
pub fn json_mutate(
    req: &Value,
    id: &str,
    path: &Path,
    transform: impl FnOnce(&mut Value) -> Result<()>,
) -> Result<Value> {
    mutate(req, id, path, |old| {
        let mut v = match old {
            Some(s) => serde_json::from_str::<Value>(s)
                .map_err(|e| format!("Invalid JSON in {}: {e}", path.display()))?,
            None => json!({}),
        };
        if !v.is_object() {
            return Err("Expected JSON object".into());
        }
        let original = v.clone();
        transform(&mut v)?;
        if v == original {
            return Ok(old.map(str::to_string));
        }
        Ok(Some(format!(
            "{}\n",
            serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?
        )))
    })
}
pub fn socket_call(path: &Path, req: &Value) -> Result<Value> {
    let mut s = UnixStream::connect(path).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_millis(750)))
        .map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(Duration::from_millis(750)))
        .map_err(|e| e.to_string())?;
    writeln!(s, "{req}").map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(s)
        .take(4 * 1024 * 1024)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if !line.ends_with('\n') {
        return Err("Socket closed before newline".into());
    }
    serde_json::from_str(&line).map_err(|e| e.to_string())
}
fn pid_key(key: &str) -> bool {
    key.ends_with("_pid") || key.ends_with("Pid")
}
fn positive_pid(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .filter(|p| *p > 0 && *p <= i32::MAX as u64)
        .and_then(|p| u32::try_from(p).ok())
}
pub fn explicit_pid(req: &Value) -> Result<Option<u32>> {
    let mut claims = Vec::new();
    if !req["endpoint_pid"].is_null() {
        claims.push(&req["endpoint_pid"]);
    }
    for meta in [&req["meta"], &req["_meta"]] {
        if let Some(object) = meta.as_object() {
            claims.extend(
                object
                    .iter()
                    .filter(|(k, _)| {
                        pid_key(k) && !matches!(k.as_str(), "parent_pid" | "http_client_pid")
                    })
                    .map(|(_, v)| v),
            );
        }
    }
    let mut ids = Vec::new();
    for value in claims {
        ids.push(positive_pid(value).ok_or("Invalid endpoint PID")?);
    }
    ids.sort();
    ids.dedup();
    if ids.len() > 1 {
        return Err("Conflicting caller metadata".into());
    }
    Ok(ids.pop())
}
pub fn caller_pid_roots(req: &Value) -> Result<Vec<u32>> {
    if let Some(p) = explicit_pid(req)? {
        return Ok(vec![p]);
    }
    fn walk(value: &Value, ids: &mut Vec<u32>) {
        match value {
            Value::Object(object) => {
                for (k, v) in object {
                    if pid_key(k) {
                        if let Some(p) = positive_pid(v) {
                            if !ids.contains(&p) {
                                ids.push(p);
                            }
                        }
                    }
                    if v.is_object() || v.is_array() {
                        walk(v, ids);
                    }
                }
            }
            Value::Array(values) => {
                for v in values {
                    walk(v, ids);
                }
            }
            _ => {}
        }
    }
    let mut ids = vec![];
    for meta in [&req["meta"], &req["_meta"]] {
        if let Some(o) = meta.as_object() {
            for v in o.values() {
                if v.is_object() || v.is_array() {
                    walk(v, &mut ids);
                }
            }
        }
    }
    if !ids.is_empty() {
        return Ok(ids);
    }
    if let Some(p) = positive_pid(&req["http_client_pid"])
        .or_else(|| positive_pid(&req["meta"]["http_client_pid"]))
    {
        return Ok(vec![p]);
    }
    if req["http_transport"] == true || req["meta"]["http_transport"] == true {
        return Ok(vec![]);
    }
    // SAFETY: getppid only reads the current process's parent ID.
    Ok(vec![unsafe { libc::getppid() } as u32])
}
pub fn process_start(pid: u32) -> Option<String> {
    ps_field(pid, "lstart=").ok()
}
pub fn process_parent(p: u32) -> Option<u32> {
    ps_field(p, "ppid=")
        .ok()?
        .parse()
        .ok()
        .filter(|parent| *parent > 0 && *parent != p)
}
pub fn pid(req: &Value) -> Option<u32> {
    explicit_pid(req)
        .ok()
        .flatten()
        .or_else(|| positive_pid(&req["http_client_pid"]))
        .or_else(|| {
            let mut context = req.clone();
            context["http_transport"] = json!(true);
            caller_pid_roots(&context)
                .ok()
                .and_then(|roots| roots.into_iter().next())
        })
}
pub fn verify_expected_identity(caller: &Value, meta: &Value) -> Result<()> {
    if let Some(expected) = str_at(meta, "expected_harness") {
        if caller["harness"] != expected {
            return Err("Conflicting caller metadata".into());
        }
    }
    if let Some(expected) = str_at(meta, "expected_session_id") {
        if caller["session_id"] != expected {
            return Err("Conflicting caller metadata".into());
        }
    }
    Ok(())
}
pub fn verify(caller: &Value, req: &Value) -> Result<()> {
    verify_expected_identity(caller, &req["meta"])?;
    verify_expected_identity(caller, &req["_meta"])?;
    if let Some(expected) = str_at(req, "explicit_session_id") {
        if caller["session_id"] != expected {
            return Err("Conflicting caller metadata".into());
        }
    }
    Ok(())
}
fn ps_field(pid: u32, field: &str) -> Result<String> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", field])
        .output()
        .map_err(|_| "Cannot inspect cached endpoint process")?;
    if !output.status.success() {
        return Err("Cached endpoint process is no longer running".into());
    }
    let text = String::from_utf8(output.stdout).map_err(|_| "Invalid process description")?;
    let text = text.trim();
    if text.is_empty() {
        return Err("Cached endpoint process is no longer running".into());
    }
    Ok(text.to_string())
}
pub fn process_identity(pid: u32) -> Result<Value> {
    // The command detects a PID reused by another application; start time also binds
    // a cache entry to one invocation when the replacement runs the same application.
    let started_at = ps_field(pid, "lstart=")?;
    let command = ps_field(pid, "command=")?;
    if ps_field(pid, "lstart=")? != started_at {
        return Err("Endpoint process changed during inspection".into());
    }
    Ok(json!({"command":command,"started_at":started_at}))
}
pub fn remember_process(mut caller: Value) -> Value {
    if let Some(pid) = caller["endpoint_pid"]
        .as_u64()
        .and_then(|p| u32::try_from(p).ok())
        .filter(|p| *p > 0)
    {
        if let Ok(identity) = process_identity(pid) {
            caller["process_identity"] = identity;
        }
    }
    caller
}
// Concrete plugins supply their own process-command predicate and session hint.
// The host owns cache storage and passes a previously successful caller unchanged.
pub fn cached_caller(
    req: &Value,
    harness: &str,
    session_hint: Option<&str>,
    matches_command: impl FnOnce(&str) -> bool,
) -> std::result::Result<Option<Value>, Value> {
    if req["require_open"] != false || req.get("cached_caller").is_none() {
        return Ok(None);
    }
    let cached = &req["cached_caller"];
    if !cached.is_object() || cached["harness"] != harness {
        return Err(error(
            "INVALID_INPUT",
            "Cached caller harness does not match the plugin",
        ));
    }
    let session = str_at(cached, "session_id")
        .ok_or_else(|| error("INVALID_INPUT", "Cached session identity is missing"))?;
    validate_session(session).map_err(|e| error("INVALID_INPUT", e))?;
    let pid = pid(req)
        .ok_or_else(|| error("BINDING_REQUIRED", "Cached caller requires an endpoint PID"))?;
    if cached["endpoint_pid"].as_u64() != Some(u64::from(pid)) {
        return Err(error(
            "INVALID_INPUT",
            "Cached caller endpoint PID does not match",
        ));
    }
    verify(cached, req).map_err(|e| error("INVALID_INPUT", e))?;
    if session_hint.map(|s| s != session).unwrap_or(false) {
        return Err(error("INVALID_INPUT", "Conflicting caller metadata"));
    }
    let current = process_identity(pid).map_err(|e| error("BINDING_REQUIRED", e))?;
    if !matches_command(
        str_at(&current, "command")
            .ok_or_else(|| error("BINDING_REQUIRED", "Endpoint command is unavailable"))?,
    ) {
        return Err(error(
            "BINDING_REQUIRED",
            "Cached endpoint process command does not match the plugin",
        ));
    }
    let previous = &cached["process_identity"];
    if str_at(previous, "command").is_none() || str_at(previous, "started_at").is_none() {
        return Err(error(
            "BINDING_REQUIRED",
            "Cached endpoint process identity is missing; resolve it while available",
        ));
    }
    if previous["command"] != current["command"] || previous["started_at"] != current["started_at"]
    {
        return Err(error(
            "BINDING_REQUIRED",
            "Cached endpoint process command or start time changed",
        ));
    }
    Ok(Some(cached.clone()))
}
// ps exposes a command line rather than argv. Inspect only the program and its
// immediate interpreter script; arbitrary later arguments never identify an app.
pub fn process_entrypoint(command: &str) -> Option<(&str, Option<&str>)> {
    let mut words = command.split_whitespace();
    let program = words.next()?.trim_matches(['\'', '"']);
    let basename = Path::new(program).file_name()?.to_str()?;
    if matches!(basename, "node" | "nodejs" | "bun" | "deno") {
        while let Some(word) = words.next() {
            if matches!(word, "-e" | "--eval" | "-p" | "--print")
                || word.starts_with("--eval=")
                || word.starts_with("--print=")
            {
                return None;
            }
            if matches!(
                word,
                "-r" | "--require" | "--import" | "--loader" | "--experimental-loader"
            ) {
                words.next()?;
                continue;
            }
            if matches!(basename, "bun" | "deno") && word == "run" {
                continue;
            }
            if word == "--" {
                return Some((program, Some(words.next()?.trim_matches(['\'', '"']))));
            }
            if !word.starts_with('-') {
                return Some((program, Some(word.trim_matches(['\'', '"']))));
            }
        }
        None
    } else {
        Some((program, None))
    }
}
pub fn normalize_repo(origin: &str) -> Option<String> {
    let remote = origin.trim().trim_end_matches('/').trim_end_matches(".git");
    let path = if let Some((_, rest)) = remote.split_once("://") {
        rest.split_once('/')?.1
    } else if remote.split_once('@').is_some() {
        remote.split_once(':')?.1
    } else {
        return None;
    };
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}
pub fn run_command_with_timeout(
    mut command: std::process::Command,
    timeout: Duration,
) -> Result<Option<std::process::Output>> {
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map(Some)
                    .map_err(|e| e.to_string())
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
            Ok(None) => {}
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn git_value(cwd: &str, args: &[&str]) -> Option<String> {
    use std::process::{Command, Stdio};
    let mut command = Command::new("git");
    command
        .args(["-C", cwd])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
    ] {
        command.env_remove(key);
    }
    let output = run_command_with_timeout(command, Duration::from_secs(1)).ok()??;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}
pub fn caller(id: &str, sid: &str, cwd: &str, pid: Option<u32>, title: Option<&str>) -> Value {
    let fallback_cwd = if cwd.trim().is_empty() {
        std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    } else {
        None
    };
    let cwd = fallback_cwd.as_deref().unwrap_or(cwd);
    let repo = git_value(cwd, &["remote", "get-url", "origin"]).and_then(|s| normalize_repo(&s));
    let branch = git_value(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .or_else(|| git_value(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]));
    json!({"harness":id,"session_id":sid,"cwd":cwd,"repo":repo,"branch":branch,"title":title.unwrap_or_else(||Path::new(cwd).file_name().and_then(|s|s.to_str()).unwrap_or("session")),"endpoint_pid":pid})
}
pub fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 checks existence without changing the process.
    unsafe {
        libc::kill(pid as i32, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

// Persistent plugins keep stdin independent of blocking network work. EOF kills all owned threads.
pub fn stdin_watchdog() -> std::sync::mpsc::Receiver<Value> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(s) => match serde_json::from_str(&s) {
                    Ok(v) => {
                        if tx.send(v).is_err() {
                            break;
                        }
                    }
                    Err(e) => eprintln!("Invalid plugin request: {e}"),
                },
                Err(_) => break,
            }
        }
        std::process::exit(0);
    });
    rx
}

#[link(name = "sqlite3")]
extern "C" {
    fn sqlite3_open_v2(
        p: *const libc::c_char,
        db: *mut *mut libc::c_void,
        flags: libc::c_int,
        vfs: *const libc::c_char,
    ) -> libc::c_int;
    fn sqlite3_close(db: *mut libc::c_void) -> libc::c_int;
    fn sqlite3_busy_timeout(db: *mut libc::c_void, ms: libc::c_int) -> libc::c_int;
    fn sqlite3_progress_handler(
        db: *mut libc::c_void,
        steps: libc::c_int,
        callback: Option<unsafe extern "C" fn(*mut libc::c_void) -> libc::c_int>,
        arg: *mut libc::c_void,
    );
    fn sqlite3_exec(
        db: *mut libc::c_void,
        sql: *const libc::c_char,
        cb: Option<
            unsafe extern "C" fn(
                *mut libc::c_void,
                libc::c_int,
                *mut *mut libc::c_char,
                *mut *mut libc::c_char,
            ) -> libc::c_int,
        >,
        arg: *mut libc::c_void,
        err: *mut *mut libc::c_char,
    ) -> libc::c_int;
    fn sqlite3_free(p: *mut libc::c_void);
}
unsafe extern "C" fn collect(
    arg: *mut libc::c_void,
    n: libc::c_int,
    vals: *mut *mut libc::c_char,
    names: *mut *mut libc::c_char,
) -> libc::c_int {
    // SAFETY: sqlite3_exec invokes this synchronously with n valid pointers and our live Vec.
    let rows = unsafe { &mut *(arg as *mut Vec<Value>) };
    let mut row = serde_json::Map::new();
    for i in 0..n as isize {
        let key = unsafe { std::ffi::CStr::from_ptr(*names.offset(i)) }
            .to_string_lossy()
            .into_owned();
        let p = unsafe { *vals.offset(i) };
        let v = if p.is_null() {
            Value::Null
        } else {
            Value::String(
                unsafe { std::ffi::CStr::from_ptr(p) }
                    .to_string_lossy()
                    .into_owned(),
            )
        };
        row.insert(key, v);
    }
    rows.push(Value::Object(row));
    0
}
/// Extract a bounded protocol diagnostic without exposing the JSON envelope.
pub fn response_diagnostic(response: &Value, fallback: &str) -> String {
    let error = &response["error"];
    let code = response["error_code"]
        .as_str()
        .or_else(|| error["code"].as_str())
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        });
    let message = response["message"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| error.as_str())
        .or_else(|| error["message"].as_str())
        .unwrap_or(fallback);
    let message = bounded_diagnostic(message, fallback);
    code.map(|c| format!("{c}: {message}")).unwrap_or(message)
}
pub fn bounded_diagnostic(message: &str, fallback: &str) -> String {
    let message = message.trim();
    if message.starts_with('{') || message.starts_with('[') {
        return fallback.into();
    }
    let text: String = message
        .chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .take(512)
        .collect();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        fallback.into()
    } else {
        text
    }
}

/// One read-only connection and one shared deadline per discovery tick.
/// The handle stays on its creating thread; Drop closes it after all callbacks return.
pub struct ReadOnlySqlite {
    db: std::cell::Cell<*mut libc::c_void>,
    uri: std::ffi::CString,
    deadline: std::time::Instant,
}
unsafe extern "C" fn sqlite_deadline(arg: *mut libc::c_void) -> libc::c_int {
    // SAFETY: rows() installs this pointer only for its synchronous sqlite3_exec,
    // then removes the callback before moving or closing the owning reader.
    let deadline = unsafe { &*(arg as *const std::time::Instant) };
    i32::from(std::time::Instant::now() >= *deadline)
}
fn sqlite_diagnostic(rc: i32) -> String {
    match rc & 255 {
        5 | 6 => "Session database is busy",
        9 => "Session database deadline exceeded",
        10 | 14 => "Session database is unavailable; check its location and permissions",
        11 | 26 => "Session database is invalid",
        1 => "Session database schema is unavailable",
        _ => "Session database query failed",
    }
    .into()
}
impl ReadOnlySqlite {
    pub fn open(path: &Path, budget: Duration) -> Result<Self> {
        let deadline = std::time::Instant::now() + budget;
        if !path
            .try_exists()
            .map_err(|_| "Cannot inspect session database")?
        {
            return Err("Session database was not found".into());
        }
        use std::os::unix::ffi::OsStrExt;
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|_| "Cannot locate session database")?
                .join(path)
        };
        let mut uri = String::from("file:");
        for byte in absolute.as_os_str().as_bytes() {
            if byte.is_ascii_alphanumeric() || matches!(*byte, b'/' | b'-' | b'_' | b'.' | b'~') {
                uri.push(*byte as char);
            } else {
                uri.push_str(&format!("%{byte:02X}"));
            }
        }
        let path = std::ffi::CString::new(uri).map_err(|_| "Invalid session database path")?;
        loop {
            let mut db = std::ptr::null_mut();
            // SAFETY: path is a live C string, the output pointer is valid, and
            // every failed open is closed before retrying. Flags request read-only.
            let rc = unsafe { sqlite3_open_v2(path.as_ptr(), &mut db, 1 | 0x40, std::ptr::null()) };
            if rc == 0 {
                let reader = Self {
                    db: std::cell::Cell::new(db),
                    uri: path,
                    deadline,
                };
                // Force WAL initialization here as well as at query time.
                reader.rows("PRAGMA schema_version")?;
                return Ok(reader);
            }
            if !db.is_null() {
                unsafe {
                    sqlite3_close(db);
                }
            }
            if rc & 255 == 14 {
                // Some SQLite builds initialize WAL during open; others defer it
                // until the first statement. Both use the same lockful fallback.
                let reader = Self {
                    db: std::cell::Cell::new(std::ptr::null_mut()),
                    uri: path,
                    deadline,
                };
                reader.prepare_wal_reader()?;
                reader.rows("PRAGMA schema_version")?;
                return Ok(reader);
            }
            if !matches!(rc, 5 | 6) || std::time::Instant::now() >= deadline {
                return Err(sqlite_diagnostic(rc));
            }
            std::thread::sleep(
                Duration::from_millis(10)
                    .min(deadline.saturating_duration_since(std::time::Instant::now())),
            );
        }
    }
    pub fn rows(&self, sql: &str) -> Result<Vec<Value>> {
        let sql = std::ffi::CString::new(sql).map_err(|e| e.to_string())?;
        loop {
            let remaining = self
                .deadline
                .saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err("Session database deadline exceeded".into());
            }
            let mut rows: Vec<Value> = Vec::new();
            let mut err = std::ptr::null_mut();
            // SAFETY: this owns the connection, SQL lives through exec, and the
            // synchronous callback exclusively borrows rows. SQLite owns err.
            let rc = unsafe {
                sqlite3_busy_timeout(self.db.get(), remaining.as_millis().min(50) as i32);
                sqlite3_progress_handler(
                    self.db.get(),
                    1000,
                    Some(sqlite_deadline),
                    &self.deadline as *const _ as *mut libc::c_void,
                );
                let rc = sqlite3_exec(
                    self.db.get(),
                    sql.as_ptr(),
                    Some(collect),
                    &mut rows as *mut _ as *mut libc::c_void,
                    &mut err,
                );
                sqlite3_progress_handler(self.db.get(), 0, None, std::ptr::null_mut());
                rc
            };
            if !err.is_null() {
                // SAFETY: sqlite3_exec allocated this diagnostic, which never escapes.
                unsafe {
                    sqlite3_free(err.cast());
                }
            }
            if rc == 0 {
                return Ok(rows);
            }
            if rc & 255 == 14 {
                self.prepare_wal_reader()?;
            } else if !matches!(rc & 255, 5 | 6) {
                return Err(sqlite_diagnostic(rc));
            }
            std::thread::sleep(
                Duration::from_millis(5).min(
                    self.deadline
                        .saturating_duration_since(std::time::Instant::now()),
                ),
            );
        }
    }
    fn prepare_wal_reader(&self) -> Result<()> {
        let mut db = std::ptr::null_mut();
        // SAFETY: the URI and output pointer live through open. No CREATE flag:
        // SQLite may initialize WAL/SHM locking, but must not create a missing DB.
        let rc = unsafe { sqlite3_open_v2(self.uri.as_ptr(), &mut db, 2 | 0x40, std::ptr::null()) };
        if rc != 0 {
            if !db.is_null() {
                unsafe {
                    sqlite3_close(db);
                }
            }
            return Err(sqlite_diagnostic(rc));
        }
        // A read/write-capable handle is needed to initialize a missing SHM.
        // query_only prohibits database changes while retaining SQLite WAL locks;
        // immutable/nolock would incorrectly ignore committed WAL frames.
        let rc = unsafe {
            sqlite3_exec(
                db,
                c"PRAGMA query_only=ON".as_ptr(),
                None,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if rc != 0 {
            unsafe {
                sqlite3_close(db);
            }
            return Err(sqlite_diagnostic(rc));
        }
        let previous = self.db.replace(db);
        // SAFETY: rows() has finished its synchronous exec; no statements escape.
        if !previous.is_null() {
            unsafe {
                sqlite3_close(previous);
            }
        }
        Ok(())
    }
}
impl Drop for ReadOnlySqlite {
    fn drop(&mut self) {
        // SAFETY: this uniquely owns the handle when present; no statements escape rows().
        if !self.db.get().is_null() {
            unsafe {
                sqlite3_close(self.db.get());
            }
        }
    }
}
pub fn sqlite_rows(path: &Path, sql: &str) -> Result<Vec<Value>> {
    ReadOnlySqlite::open(path, Duration::from_millis(1200))?.rows(sql)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn read_only_sqlite_initializes_wal_without_shm_and_preserves_committed_frames() -> Result<()> {
        let root =
            std::env::temp_dir().join(format!("cito-reader-{}", crate::plugins::random_uuid_v4()));
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let result = (|| {
            let source = root.join("writer.sqlite");
            let writer = crate::database::Database::open(&source)?;
            writer.execute("PRAGMA journal_mode=WAL; CREATE TABLE items(value TEXT); INSERT INTO items VALUES('checkpointed'); PRAGMA wal_checkpoint(TRUNCATE)",&[])?;
            for nonempty in [false, true] {
                if nonempty {
                    writer.execute("INSERT INTO items VALUES('committed frame')", &[])?;
                }
                let path = root.join(format!("copy #?%{nonempty}.sqlite"));
                std::fs::copy(&source, &path).map_err(|e| e.to_string())?;
                if nonempty {
                    std::fs::copy(
                        source.with_extension("sqlite-wal"),
                        path.with_extension("sqlite-wal"),
                    )
                    .map_err(|e| e.to_string())?;
                }
                assert!(!path.with_extension("sqlite-shm").exists());
                let reader = ReadOnlySqlite::open(&path, Duration::from_secs(1))?;
                let rows = reader.rows("SELECT value FROM items ORDER BY rowid")?;
                assert_eq!(rows.len(), if nonempty { 2 } else { 1 });
                assert_eq!(rows[0]["value"], "checkpointed");
                if nonempty {
                    assert_eq!(rows[1]["value"], "committed frame");
                }
                assert!(reader.rows("PRAGMA table_info(absent)")?.is_empty());
            }
            let limited = ReadOnlySqlite::open(&source, Duration::from_millis(30))?;
            assert_eq!(limited.rows("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n").unwrap_err(),"Session database deadline exceeded");
            assert_eq!(
                ReadOnlySqlite::open(&root.join("absent.sqlite"), Duration::from_millis(30))
                    .err()
                    .ok_or("Expected missing DB")?,
                "Session database was not found"
            );
            let invalid = root.join("invalid.sqlite");
            std::fs::write(&invalid, "not a database").map_err(|e| e.to_string())?;
            let failure = ReadOnlySqlite::open(&invalid, Duration::from_millis(30))
                .err()
                .ok_or("Expected invalid DB error")?;
            assert_eq!(failure, "Session database is invalid");
            assert!(!failure.contains("SQLite") && !failure.contains("not a database"));
            Ok(())
        })();
        std::fs::remove_dir_all(root).map_err(|e| e.to_string())?;
        result
    }
    #[test]
    fn structured_diagnostics_bound_envelopes_and_control_characters() {
        assert_eq!(
            response_diagnostic(
                &json!({"ok":false,"error_code":"OFFLINE","message":"Retry\n later"}),
                "Failed"
            ),
            "OFFLINE: Retry later"
        );
        assert_eq!(
            response_diagnostic(
                &json!({"error":{"code":"UNAVAILABLE","message":"Unavailable"}}),
                "Failed"
            ),
            "UNAVAILABLE: Unavailable"
        );
        assert_eq!(
            response_diagnostic(&json!({"error":{"unexpected":[1,2]}}), "Failed"),
            "Failed"
        );
        assert_eq!(bounded_diagnostic(&"x".repeat(10000), "Failed").len(), 512);
    }
    #[test]
    fn mutation_preserves_symlink_target_mode_and_absent_file_cleanliness() {
        let root = std::env::temp_dir().join(format!(
            "cito-mutate-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let target = root.join("settings");
        let link = root.join("alias");
        fs::write(&target, "original").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        mutate(&json!({"op":"setup_apply"}), "fixture", &link, |old| {
            assert_eq!(old, Some("original"));
            Ok(Some("updated".into()))
        })
        .unwrap();
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(&target).unwrap(), "updated");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        mutate(&json!({"op":"setup_remove"}), "fixture", &link, |_| {
            Ok(None)
        })
        .unwrap();
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!target.exists());
        assert!(!root.join("settings.cito.lock").exists());
        let missing = root.join("missing");
        mutate(&json!({"op":"setup_remove"}), "fixture", &missing, |_| {
            Ok(None)
        })
        .unwrap();
        assert!(!root.join("missing.cito.lock").exists());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        let dangling = root.join("dangling");
        std::os::unix::fs::symlink("new/parent/settings", &dangling).unwrap();
        mutate(&json!({"op":"setup_apply"}), "fixture", &dangling, |old| {
            assert_eq!(old, None);
            Ok(Some("created through link".into()))
        })
        .unwrap();
        assert!(fs::symlink_metadata(&dangling)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_to_string(root.join("new/parent/settings")).unwrap(),
            "created through link"
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn remove_sidecar_and_absent_file_creation_share_one_mutation_owner() {
        let root =
            std::env::temp_dir().join(format!("cito-lock-{}-{}", std::process::id(), now_ms()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("settings");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
        let workers: Vec<_> = (0..12)
            .map(|_| {
                let (path, barrier) = (path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..8 {
                        mutate(&json!({"op":"setup_remove"}), "fixture", &path, |old| {
                            let n: usize = old.unwrap_or("0").parse().unwrap();
                            std::thread::sleep(Duration::from_millis(1));
                            Ok(Some((n + 1).to_string()))
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "96");
        assert!(!root.join("settings.cito.lock").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn interpreter_entrypoint_ignores_loader_values_and_rejects_eval() {
        assert_eq!(process_entrypoint("node --require /tmp/preload.js --experimental-strip-types /tmp/program.js --session other"), Some(("node", Some("/tmp/program.js"))));
        assert_eq!(process_entrypoint("node -e /tmp/program.js"), None);
        assert_eq!(
            process_entrypoint("bun run -- /tmp/program.js"),
            Some(("bun", Some("/tmp/program.js")))
        );
    }
}
