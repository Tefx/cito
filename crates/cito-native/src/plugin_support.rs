#![allow(dead_code)]
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
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

// Each file mutation locks a stable sibling inode before reading; rename never replaces the lock.
pub struct FileLock(File);
impl FileLock {
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| e.to_string())?; // SAFETY: flock only borrows this live file descriptor; Drop releases it.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Self(f))
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
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else { return };
    let lock_name = format!("{name}.cito.lock");
    let bak_prefix = format!("{name}.bak.");
    let tmp_prefix = format!("{name}.tmp-");
    if let Ok(entries) = fs::read_dir(parent) {
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().into_owned();
            if fname == lock_name || fname.starts_with(&bak_prefix) || fname.starts_with(&tmp_prefix) {
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
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    let r = (|| {
        f.write_all(bytes).map_err(|e| e.to_string())?;
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
pub fn mutate(
    req: &Value,
    id: &str,
    path: &Path,
    transform: impl FnOnce(Option<&str>) -> Result<Option<String>>,
) -> Result<Value> {
    let dry = req["dry_run"] == true;
    let remove = req["op"] == "setup_remove";
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
        let next = transform(old.as_deref())?;
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
        (status, backup)
    };
    if !dry {
        let _ = fs::remove_file(&lock_path);
        if remove {
            clean_sidecars(path);
        }
    }
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
#[cfg(test)]
mod tests {
    use super::process_entrypoint;
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
    let mut child = command.spawn().ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
        if start.elapsed() > Duration::from_secs(1) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let output = child.wait_with_output().ok()?;
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
pub fn sqlite_rows(path: &Path, sql: &str) -> Result<Vec<Value>> {
    let p = std::ffi::CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
    let sql = std::ffi::CString::new(sql).map_err(|e| e.to_string())?;
    let mut db = std::ptr::null_mut();
    let mut rows: Vec<Value> = Vec::new(); // SAFETY: C strings live for the calls, db is closed on every path; callback only borrows rows synchronously.
    unsafe {
        if sqlite3_open_v2(p.as_ptr(), &mut db, 1, std::ptr::null()) != 0 {
            if !db.is_null() {
                sqlite3_close(db);
            }
            return Err("Cannot open session database".into());
        }
        let mut err = std::ptr::null_mut();
        let rc = sqlite3_exec(
            db,
            sql.as_ptr(),
            Some(collect),
            &mut rows as *mut _ as *mut libc::c_void,
            &mut err,
        );
        let message = if err.is_null() {
            None
        } else {
            let s = std::ffi::CStr::from_ptr(err).to_string_lossy().into_owned();
            sqlite3_free(err as *mut libc::c_void);
            Some(s)
        };
        sqlite3_close(db);
        if rc != 0 {
            return Err(message.unwrap_or_else(|| "Session database query failed".into()));
        }
    }
    Ok(rows)
}
