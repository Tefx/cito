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
fn clean_sidecars(path: &Path) {
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
    static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    tmp_name.push(format!(
        ".tmp-{}-{}-{}",
        std::process::id(),
        now_ms(),
        NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
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
fn socket_io_error(error: std::io::Error) -> String {
    if matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) {
        "Socket request timed out".into()
    } else {
        error.to_string()
    }
}

pub fn socket_call(path: &Path, req: &Value) -> Result<Value> {
    let mut s = UnixStream::connect(path).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_millis(750)))
        .map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(Duration::from_millis(750)))
        .map_err(|e| e.to_string())?;
    writeln!(s, "{req}").map_err(socket_io_error)?;
    let mut line = String::new();
    BufReader::new(s)
        .take(4 * 1024 * 1024)
        .read_line(&mut line)
        .map_err(socket_io_error)?;
    if !line.ends_with('\n') {
        return Err("Socket closed before newline".into());
    }
    serde_json::from_str(&line).map_err(|e| e.to_string())
}
// Inspect sibling bridges with at most eight workers sharing a dynamic queue.
// Return completed results in PID order within the provider's discovery budget;
// unfinished socket calls may finish independently, but cannot hold up siblings.
pub fn inspect_socket_endpoints(
    req: &Value,
    prefix: &str,
    inspect: impl Fn(u32, &Path) -> Option<Value> + Send + Sync + 'static,
) -> Result<Vec<(u32, Value)>> {
    use std::sync::{atomic::AtomicUsize, atomic::Ordering, mpsc, Arc};
    let deadline = std::time::Instant::now() + Duration::from_millis(1100);
    let entries = match fs::read_dir(state(req).join("sockets")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut endpoints = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        if let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix(prefix))
            .and_then(|name| name.strip_suffix(".sock"))
            .and_then(|pid| pid.parse::<u32>().ok())
        {
            endpoints.push((pid, entry.path()));
        }
    }
    endpoints.sort_by_key(|(pid, _)| *pid);
    let endpoints = Arc::new(endpoints);
    let next = Arc::new(AtomicUsize::new(0));
    let inspect = Arc::new(inspect);
    let (tx, rx) = mpsc::channel();
    for _ in 0..endpoints.len().min(8) {
        let endpoints = endpoints.clone();
        let next = next.clone();
        let inspect = inspect.clone();
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("endpoint-inspection".into())
            .spawn(move || {
                while std::time::Instant::now() < deadline {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((pid, path)) = endpoints.get(index) else {
                        break;
                    };
                    if tx.send((*pid, inspect(*pid, path))).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| format!("Cannot start endpoint inspection worker: {e}"))?;
    }
    drop(tx);
    let mut results = Vec::new();
    while let Ok((pid, value)) =
        rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
    {
        if let Some(value) = value {
            results.push((pid, value));
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
    }
    results.sort_by_key(|(pid, _)| *pid);
    Ok(results)
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
    for meta in [
        &req["meta"],
        &req["_meta"],
        &req["_stdio"],
        &req["meta"]["_stdio"],
        &req["_meta"]["_stdio"],
    ] {
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
    #[cfg(target_os = "macos")]
    {
        process_info(pid)
            .ok()
            .map(|info| format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec))
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()?
            .rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(19)
            .map(str::to_owned)
    }
}
pub fn process_parent(p: u32) -> Option<u32> {
    #[cfg(target_os = "macos")]
    let parent = process_info(p).ok()?.pbi_ppid;
    #[cfg(not(target_os = "macos"))]
    let parent = std::fs::read_to_string(format!("/proc/{p}/stat"))
        .ok()?
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    (parent > 0 && parent != p).then_some(parent)
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
fn verify_expected_identity(caller: &Value, meta: &Value) -> Result<()> {
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
#[cfg(target_os = "macos")]
fn process_info(pid: u32) -> Result<libc::proc_bsdinfo> {
    // SAFETY: proc_pidinfo writes exactly the size of this initialized C structure.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    if unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    } != size
    {
        return Err("Cannot inspect endpoint process".into());
    }
    Ok(info)
}
#[cfg(target_os = "macos")]
fn descriptor_path(pid: u32, fd: i32) -> Option<(PathBuf, libc::mode_t)> {
    // Darwin's vnode_fdinfowithpath is proc_fileinfo followed by vnode_info_path.
    #[repr(C)]
    struct VnodeFdPath {
        open_flags: u32,
        status: u32,
        offset: i64,
        kind: i32,
        guard_flags: u32,
        vnode: libc::vnode_info_path,
    }
    // SAFETY: the initialized repr(C) buffer exactly matches sys/proc_info.h.
    let mut info: VnodeFdPath = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    // PROC_PIDFDVNODEPATHINFO = 2; libc exposes the function but not this flavor.
    if unsafe {
        libc::proc_pidfdinfo(
            pid as i32,
            fd,
            2,
            (&mut info as *mut VnodeFdPath).cast(),
            size,
        )
    } != size
    {
        return None;
    }
    let bytes: Vec<u8> = info
        .vnode
        .vip_path
        .iter()
        .flatten()
        .map(|c| *c as u8)
        .take_while(|c| *c != 0)
        .collect();
    use std::os::unix::ffi::OsStringExt;
    Some((
        std::ffi::OsString::from_vec(bytes).into(),
        info.vnode.vip_vi.vi_stat.vst_mode,
    ))
}
#[cfg(not(target_os = "macos"))]
fn descriptor_path(pid: u32, fd: i32) -> Option<(PathBuf, libc::mode_t)> {
    use std::os::unix::fs::MetadataExt;
    let link = format!("/proc/{pid}/fd/{fd}");
    Some((
        std::fs::read_link(&link).ok()?,
        std::fs::metadata(link).ok()?.mode() as libc::mode_t,
    ))
}
pub fn process_has_tty(pid: u32) -> bool {
    let terminal = |(path, mode): &(PathBuf, libc::mode_t)| {
        let path = path.to_string_lossy();
        *mode & libc::S_IFMT == libc::S_IFCHR
            && (path.starts_with("/dev/tty") || path.starts_with("/dev/pts/"))
    };
    let Some(stdin) = descriptor_path(pid, 0) else {
        return false;
    };
    if terminal(&stdin) {
        return true;
    }
    // Null, pipes and sockets cannot borrow the inherited stdout terminal.
    if stdin.0 == Path::new("/dev/null")
        || !stdin.0.is_absolute()
        || matches!(stdin.1 & libc::S_IFMT, libc::S_IFIFO | libc::S_IFSOCK)
    {
        return false;
    }
    descriptor_path(pid, 1).as_ref().is_some_and(terminal)
}

fn same_user_process(pid: u32) -> bool {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: geteuid only reads process credentials.
        process_info(pid).is_ok_and(|info| info.pbi_uid == unsafe { libc::geteuid() })
    }
    #[cfg(not(target_os = "macos"))]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(format!("/proc/{pid}"))
            .is_ok_and(|meta| meta.uid() == unsafe { libc::geteuid() })
    }
}

/// Same-user ancestry is bounded and stops when process inspection is unavailable.
pub fn has_harness_ancestor(pid: u32, matches: impl Fn(u32, &str) -> bool) -> bool {
    let mut current = pid;
    for _ in 0..16 {
        let Some(parent) = process_parent(current).filter(|p| *p > 1) else {
            break;
        };
        if !same_user_process(parent) {
            break;
        }
        if process_command(parent).is_ok_and(|command| matches(parent, &command)) {
            return true;
        }
        current = parent;
    }
    false
}

pub fn is_agent_harness(command: &str) -> bool {
    let Some((program, script)) = process_entrypoint(command) else {
        return false;
    };
    let entry = script.unwrap_or(program);
    let name = Path::new(entry)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    matches!(
        name,
        "pi" | "pi-coding-agent"
            | "pi.js"
            | "pi.mjs"
            | "claude"
            | "claude.js"
            | "claude.mjs"
            | "codex"
            | "codex.js"
            | "codex.mjs"
    ) || (entry.contains("/pi-coding-agent/") || entry.contains("/claude-code/"))
        && matches!(name, "cli.js" | "cli.mjs")
}

/// Ignore inherited endpoint hints that cross a nearer harness boundary.
pub fn enclosing_endpoint_pid(hint: Option<u32>, parent: u32) -> u32 {
    let hint = hint.filter(|p| alive(*p) && same_user_process(*p));
    let mut current = parent;
    for _ in 0..16 {
        if Some(current) == hint {
            return current;
        }
        if !same_user_process(current) {
            break;
        }
        if process_command(current).is_ok_and(|command| is_agent_harness(&command)) {
            return current;
        }
        let Some(next) = process_parent(current).filter(|p| *p > 1) else {
            break;
        };
        current = next;
    }
    hint.unwrap_or(parent)
}
pub fn process_cwd(pid: u32) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info) as i32;
        // SAFETY: the C structure is writable and path bytes are bounded by its fixed array.
        if unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                (&mut info as *mut libc::proc_vnodepathinfo).cast(),
                size,
            )
        } != size
        {
            return Err("Cannot inspect endpoint working directory".into());
        }
        let bytes = info
            .pvi_cdir
            .vip_path
            .iter()
            .flatten()
            .map(|b| *b as u8)
            .take_while(|b| *b != 0)
            .collect::<Vec<_>>();
        String::from_utf8(bytes).map_err(|_| "Invalid endpoint working directory".into())
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::fs::read_link(format!("/proc/{pid}/cwd"))
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|_| "Cannot inspect endpoint working directory".into())
    }
}
pub fn process_command(pid: u32) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as i32];
        let mut bytes = vec![0u8; 1024 * 1024];
        let mut size = bytes.len();
        // SAFETY: sysctl borrows the initialized MIB and bounded writable byte buffer.
        if unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as _,
                bytes.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        } != 0
            || size < 4
        {
            return Err("Cannot inspect endpoint command".into());
        }
        bytes.truncate(size);
        let argc = i32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]).max(0) as usize;
        let args = &bytes[4..];
        let executable_end = args
            .iter()
            .position(|b| *b == 0)
            .ok_or("Invalid endpoint command")?;
        let args = &args[executable_end..];
        let start = args.iter().position(|b| *b != 0).unwrap_or(args.len());
        Ok(args[start..]
            .split(|b| *b == 0)
            .take(argc)
            .map(String::from_utf8_lossy)
            .collect::<Vec<_>>()
            .join(" "))
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::fs::read(format!("/proc/{pid}/cmdline"))
            .map(|b| {
                String::from_utf8_lossy(&b)
                    .trim_end_matches('\0')
                    .replace('\0', " ")
            })
            .map_err(|_| "Cannot inspect endpoint command".into())
    }
}
/// Enumerate same-user processes without launching one subprocess per endpoint.
pub fn user_processes() -> Result<Vec<u32>> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: proc_listpids first reports capacity, then fills our bounded PID array.
        // libproc's PROC_ALL_PIDS is defined by sys/proc_info.h, absent from libc.
        const PROC_ALL_PIDS: u32 = 1;
        let bytes = unsafe { libc::proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0) };
        if bytes <= 0 {
            return Err("Cannot enumerate endpoint processes".into());
        }
        let mut pids = vec![0u32; bytes as usize / 4 + 128];
        let count = unsafe {
            libc::proc_listpids(
                PROC_ALL_PIDS,
                0,
                pids.as_mut_ptr().cast(),
                (pids.len() * 4) as i32,
            )
        };
        if count < 0 {
            return Err("Cannot enumerate endpoint processes".into());
        }
        pids.truncate(count as usize / 4);
        let uid = unsafe { libc::geteuid() };
        pids.retain(|pid| *pid > 0 && process_info(*pid).is_ok_and(|info| info.pbi_uid == uid));
        Ok(pids)
    }
    #[cfg(not(target_os = "macos"))]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = unsafe { libc::geteuid() };
        Ok(std::fs::read_dir("/proc")
            .map_err(|_| "Cannot enumerate endpoint processes")?
            .flatten()
            .filter(|entry| entry.metadata().is_ok_and(|m| m.uid() == uid))
            .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
            .collect())
    }
}
pub fn process_identity(pid: u32) -> Result<Value> {
    // The command detects a PID reused by another application; start time also binds
    // a cache entry to one invocation when the replacement runs the same application.
    let started_at = process_start(pid).ok_or("Cannot inspect endpoint process")?;
    let command = process_command(pid)?;
    if process_start(pid).as_deref() != Some(&started_at) {
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
#[derive(Default)]
pub struct RetryBackoff {
    failures: u32,
}
impl RetryBackoff {
    pub fn reset(&mut self) {
        self.failures = 0;
    }
    pub fn failed(&mut self) -> Duration {
        self.failures = self.failures.saturating_add(1);
        let base = 250u64.saturating_mul(1u64 << self.failures.saturating_sub(1).min(7));
        let mut random = [0u8; 2];
        let _ = getrandom::getrandom(&mut random);
        Duration::from_millis(
            (base + u16::from_ne_bytes(random) as u64 % (base / 4 + 1)).min(30_000),
        )
    }
}

/// Coalescing nonblocking wake pipe. Ownership keeps descriptors valid during poll.
pub struct WakePipe {
    read: std::os::unix::net::UnixStream,
    write: std::os::unix::net::UnixStream,
}
impl WakePipe {
    pub fn new() -> Result<Self> {
        let (read, write) = std::os::unix::net::UnixStream::pair().map_err(|e| e.to_string())?;
        read.set_nonblocking(true).map_err(|e| e.to_string())?;
        write.set_nonblocking(true).map_err(|e| e.to_string())?;
        Ok(Self { read, write })
    }
    pub fn read_fd(&self) -> i32 {
        use std::os::fd::AsRawFd;
        self.read.as_raw_fd()
    }
    pub fn write_fd(&self) -> i32 {
        use std::os::fd::AsRawFd;
        self.write.as_raw_fd()
    }
    pub fn notify(&self) {
        // SAFETY: the owned stream remains valid; a full pipe already represents a wakeup.
        unsafe {
            libc::write(self.write_fd(), [1u8].as_ptr().cast(), 1);
        }
    }
    pub fn drain(&self) {
        let mut bytes = [0u8; 256];
        // SAFETY: read borrows a writable live buffer and never blocks.
        while unsafe { libc::read(self.read_fd(), bytes.as_mut_ptr().cast(), bytes.len()) } > 0 {}
    }
}

/// Observe an owned, unreaped child without releasing its PID before cleanup.
pub fn child_exit_fd(pid: u32) -> Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    #[cfg(target_os = "macos")]
    {
        // SAFETY: kqueue returns a new descriptor and kevent borrows initialized data.
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        let change = libc::kevent {
            ident: pid as _,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        if unsafe { libc::kevent(fd, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(owned)
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: pidfd_open observes a child whose PID remains reserved until wait.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
    }
}

/// Block for descriptor readiness, preserving the absolute deadline across signals.
pub fn poll_until(fds: &mut [libc::pollfd], deadline: Option<std::time::Instant>) -> Result<bool> {
    loop {
        let timeout = deadline
            .map(|at| {
                at.saturating_duration_since(std::time::Instant::now())
                    .as_millis()
                    .saturating_add(1)
                    .min(i32::MAX as u128) as i32
            })
            .unwrap_or(-1);
        // SAFETY: poll borrows a live initialized slice for the duration of the call.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, timeout) };
        if n >= 0 {
            return Ok(n > 0);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.to_string());
        }
        if deadline.is_some_and(|at| at <= std::time::Instant::now()) {
            return Ok(false);
        }
    }
}

pub fn wait_child_exit(child: &std::process::Child, timeout: Duration) -> Result<bool> {
    use std::os::fd::AsRawFd;
    let fd = child_exit_fd(child.id())?;
    poll_until(
        &mut [libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }],
        Some(std::time::Instant::now() + timeout),
    )
}

pub fn run_command_with_timeout(
    mut command: std::process::Command,
    timeout: Duration,
) -> Result<Option<std::process::Output>> {
    use std::io::Read;
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    let deadline = std::time::Instant::now() + timeout;
    let mut child = command
        .process_group(0)
        .spawn()
        .map_err(|e| e.to_string())?;
    let result = (|| -> Result<Option<[Vec<u8>; 2]>> {
        let mut pipes = [
            child.stdout.take().map(|p| File::from(OwnedFd::from(p))),
            child.stderr.take().map(|p| File::from(OwnedFd::from(p))),
        ];
        for pipe in pipes.iter().flatten() {
            // SAFETY: each pipe remains owned until the drain completes or times out.
            unsafe {
                let flags = libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL);
                if flags < 0
                    || libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0
                {
                    return Err(std::io::Error::last_os_error().to_string());
                }
            }
        }
        let exited = child_exit_fd(child.id())?;
        let mut child_exited = false;
        let mut output = [Vec::new(), Vec::new()];
        loop {
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            for (pipe, buffer) in pipes.iter_mut().zip(&mut output) {
                if let Some(reader) = pipe {
                    let mut bytes = [0u8; 8192];
                    match reader.read(&mut bytes) {
                        Ok(0) => *pipe = None,
                        Ok(n) => buffer.extend_from_slice(&bytes[..n]),
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                            ) => {}
                        Err(e) => return Err(e.to_string()),
                    }
                }
            }
            if child_exited && pipes.iter().all(Option::is_none) {
                return Ok(Some(output));
            }
            let mut fds = [
                libc::pollfd {
                    fd: pipes[0].as_ref().map_or(-1, AsRawFd::as_raw_fd),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: pipes[1].as_ref().map_or(-1, AsRawFd::as_raw_fd),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: if child_exited { -1 } else { exited.as_raw_fd() },
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            poll_until(&mut fds, Some(deadline))?;
            child_exited |= fds[2].revents != 0;
        }
    })();
    if !matches!(result, Ok(Some(_))) {
        // SAFETY: the unreaped child reserves this owned process-group ID until wait.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }
    let status = child.wait().map_err(|e| e.to_string());
    let Some([stdout, stderr]) = result? else {
        return Ok(None);
    };
    Ok(Some(std::process::Output {
        status: status?,
        stdout,
        stderr,
    }))
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
    (!text.is_empty()).then_some(text)
}
fn git_metadata(req: &Value, cwd: &str) -> (Option<String>, Option<String>) {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::MetadataExt;
    let Some((root, marker)) = Path::new(cwd).ancestors().find_map(|path| {
        let marker = path.join(".git");
        marker.exists().then_some((path, marker))
    }) else {
        return (None, None);
    };
    let git = if marker.is_dir() {
        marker.clone()
    } else {
        let Ok(content) = std::fs::read_to_string(&marker) else {
            return (None, None);
        };
        let Some(path) = content.trim().strip_prefix("gitdir: ") else {
            return (None, None);
        };
        root.join(path)
    };
    let common = std::fs::read_to_string(git.join("commondir"))
        .ok()
        .map(|path| git.join(path.trim()))
        .unwrap_or_else(|| git.clone());
    let metadata = || {
        json!([
            marker.clone(),
            git.join("HEAD"),
            common.join("config"),
            home(req).join(".gitconfig"),
            home(req).join(".config/git/config")
        ]
        .iter()
        .map(|path| path.metadata().ok().map(|m| json!([
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime(),
            m.mtime_nsec()
        ])))
        .collect::<Vec<_>>())
    };
    let before = metadata();
    let path = state(req)
        .join("git-cache")
        .join(format!("{:x}.json", Sha256::digest(cwd.as_bytes())));
    let cached = read_json(&path).unwrap_or(Value::Null);
    // A slow reconciliation also observes indirect Git configuration includes.
    if cached["metadata"] == before
        && cached["at_ms"]
            .as_u64()
            .is_some_and(|at| now_ms().saturating_sub(at) < 300_000)
    {
        return (
            cached["repo"].as_str().map(str::to_owned),
            cached["branch"].as_str().map(str::to_owned),
        );
    }
    let repo =
        git_value(cwd, &["remote", "get-url", "origin"]).and_then(|url| normalize_repo(&url));
    let branch = git_value(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .or_else(|| git_value(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]));
    if before == metadata() && branch.is_some() {
        let _ = atomic_write(
            &path,
            json!({"metadata":before,"at_ms":now_ms(),"repo":repo,"branch":branch})
                .to_string()
                .as_bytes(),
        );
    }
    (repo, branch)
}

/// Cache a file projection by vnode identity, length, and nanosecond modification time.
/// Save only stable observations; a concurrent writer invalidates the next read.
pub fn cached_file_projection(
    req: &Value,
    namespace: &str,
    source: &Path,
    read: impl FnOnce() -> Result<Value>,
) -> Result<Value> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::MetadataExt;
    let key = || -> Result<Value> {
        let m = source.metadata().map_err(|e| e.to_string())?;
        Ok(json!([
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime(),
            m.mtime_nsec()
        ]))
    };
    let before = key()?;
    let path = state(req).join(namespace).join(format!(
        "{:x}.json",
        Sha256::digest(source.as_os_str().as_encoded_bytes())
    ));
    let cached = read_json(&path).unwrap_or(Value::Null);
    if cached["metadata"] == before {
        return Ok(cached["value"].clone());
    }
    let value = read()?;
    if key().is_ok_and(|after| after == before) {
        atomic_write(
            &path,
            json!({"metadata":before,"value":value})
                .to_string()
                .as_bytes(),
        )?;
    }
    Ok(value)
}
pub fn caller(
    req: &Value,
    id: &str,
    sid: &str,
    cwd: &str,
    pid: Option<u32>,
    title: Option<&str>,
) -> Value {
    let fallback_cwd = if cwd.trim().is_empty() {
        std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    } else {
        None
    };
    let cwd = fallback_cwd.as_deref().unwrap_or(cwd);
    let (repo, branch) = git_metadata(req, cwd);
    json!({"harness":id,"session_id":sid,"cwd":cwd,"repo":repo,"branch":branch,"title":title.unwrap_or_else(||Path::new(cwd).file_name().and_then(|s|s.to_str()).unwrap_or("session")),"endpoint_pid":pid})
}
pub fn alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    // SAFETY: signal 0 checks existence without changing the process.
    unsafe {
        libc::kill(pid as i32, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

// Persistent plugins keep stdin independent of blocking network work. EOF kills all owned threads.
pub fn stdin_watchdog() -> std::sync::mpsc::Receiver<Value> {
    let (tx, rx) = std::sync::mpsc::channel();
    stdin_events(tx, |value| value);
    rx
}
pub fn stdin_events<T: Send + 'static>(tx: std::sync::mpsc::Sender<T>, wrap: fn(Value) -> T) {
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(s) => match serde_json::from_str(&s) {
                    Ok(v) => {
                        if tx.send(wrap(v)).is_err() {
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
    wal_fallback_attempted: std::cell::Cell<bool>,
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
        {
            let mut db = std::ptr::null_mut();
            // SAFETY: path is a live C string, the output pointer is valid, and
            // every failed open is closed before retrying. Flags request read-only.
            let rc = unsafe { sqlite3_open_v2(path.as_ptr(), &mut db, 1 | 0x40, std::ptr::null()) };
            if rc == 0 {
                let reader = Self {
                    db: std::cell::Cell::new(db),
                    wal_fallback_attempted: std::cell::Cell::new(false),
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
                    wal_fallback_attempted: std::cell::Cell::new(false),
                    uri: path,
                    deadline,
                };
                reader.prepare_wal_reader()?;
                reader.rows("PRAGMA schema_version")?;
                return Ok(reader);
            }
            Err(sqlite_diagnostic(rc))
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
                sqlite3_busy_timeout(
                    self.db.get(),
                    remaining.as_millis().min(i32::MAX as u128) as i32,
                );
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
            } else {
                return Err(sqlite_diagnostic(rc));
            }
        }
    }
    fn prepare_wal_reader(&self) -> Result<()> {
        if self.wal_fallback_attempted.replace(true) {
            return Err(sqlite_diagnostic(14));
        }
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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn alive_rejects_process_group_and_out_of_range_ids() {
        assert!(!alive(0));
        assert!(!alive(u32::MAX));
        assert!(alive(std::process::id()));
    }

    #[test]
    fn native_process_queries_preserve_invocation_parent_and_cwd() -> Result<()> {
        let pid = std::process::id();
        let identity = process_identity(pid)?;
        assert_eq!(
            process_start(pid).as_deref(),
            identity["started_at"].as_str()
        );
        assert_eq!(process_identity(pid)?, identity);
        // SAFETY: getppid has no pointer or ownership requirements.
        assert_eq!(process_parent(pid), Some(unsafe { libc::getppid() } as u32));
        assert_eq!(
            std::fs::canonicalize(process_cwd(pid)?).map_err(|e| e.to_string())?,
            std::env::current_dir().map_err(|e| e.to_string())?
        );
        assert!(user_processes()?.contains(&pid));
        assert!(!str_at(&identity, "command").unwrap_or("").is_empty());
        assert!(process_identity(u32::MAX).is_err());
        Ok(())
    }

    #[test]
    fn file_projection_cache_tracks_metadata_and_skips_unchanged_parse_and_write() -> Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "cito-file-cache-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let result = (|| {
            let source = dir.join("session.jsonl");
            std::fs::write(&source, "first").map_err(|e| e.to_string())?;
            let req = json!({"state_dir":dir});
            let reads = std::cell::Cell::new(0);
            let read = || {
                reads.set(reads.get() + 1);
                Ok(json!(
                    std::fs::read_to_string(&source).map_err(|e| e.to_string())?
                ))
            };
            assert_eq!(
                cached_file_projection(&req, "cache", &source, read)?,
                "first"
            );
            assert_eq!(
                cached_file_projection(&req, "cache", &source, read)?,
                "first"
            );
            assert_eq!(reads.get(), 1);
            let replacement = dir.join("replacement");
            std::fs::write(&replacement, "other").map_err(|e| e.to_string())?;
            std::fs::rename(&replacement, &source).map_err(|e| e.to_string())?;
            assert_eq!(
                cached_file_projection(&req, "cache", &source, read)?,
                "other"
            );
            assert_eq!(
                reads.get(),
                2,
                "Same-size replacement invalidates by vnode identity"
            );
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(dir);
        result
    }

    #[test]
    fn retry_backoff_is_bounded_and_reset_restores_the_initial_interval() {
        let mut backoff = RetryBackoff::default();
        let first = backoff.failed();
        assert!((Duration::from_millis(250)..=Duration::from_millis(312)).contains(&first));
        let second = backoff.failed();
        assert!(second >= Duration::from_millis(500));
        for _ in 0..100 {
            assert!(backoff.failed() <= Duration::from_secs(30));
        }
        backoff.reset();
        assert!(backoff.failed() < Duration::from_millis(500));
    }

    #[test]
    fn wake_pipe_blocks_until_notification_and_coalesces_without_losing_readiness() -> Result<()> {
        let wake = std::sync::Arc::new(WakePipe::new()?);
        let worker = wake.clone();
        let (ready, wait) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            ready.send(()).map_err(|e| e.to_string())?;
            poll_until(
                &mut [libc::pollfd {
                    fd: worker.read_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                }],
                None,
            )
        });
        wait.recv_timeout(Duration::from_secs(1))
            .map_err(|e| e.to_string())?;
        for _ in 0..10 {
            wake.notify();
        }
        assert!(waiter.join().map_err(|_| "Wake waiter panicked")??);
        wake.drain();
        assert!(!poll_until(
            &mut [libc::pollfd {
                fd: wake.read_fd(),
                events: libc::POLLIN,
                revents: 0
            }],
            Some(std::time::Instant::now())
        )?);
        Ok(())
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
    fn endpoint_inspection_budget_preserves_healthy_siblings_in_pid_order() {
        use std::sync::{mpsc, Arc, Condvar, Mutex};
        use std::time::Instant;
        let root = std::env::temp_dir().join(format!(
            "cito-endpoints-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("sockets")).unwrap();
        for pid in (1..=9).rev() {
            fs::write(root.join(format!("sockets/fixture-{pid}.sock")), "").unwrap();
        }
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let blocked = gate.clone();
        let (finished_tx, finished_rx) = mpsc::channel();
        let started = Instant::now();
        let result = inspect_socket_endpoints(
            &json!({"env":{"CITO_STATE_DIR":root}}),
            "fixture-",
            move |pid, _| {
                if pid <= 3 {
                    let (lock, wake) = &*blocked;
                    let _guard = wake
                        .wait_timeout_while(
                            lock.lock().unwrap(),
                            Duration::from_secs(2),
                            |released| !*released,
                        )
                        .unwrap();
                    finished_tx.send(()).unwrap();
                }
                Some(json!(pid))
            },
        );
        let elapsed = started.elapsed();
        // Release and observe every pending callback before assertions or cleanup.
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        for _ in 0..3 {
            finished_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
        assert!(elapsed < Duration::from_millis(1500), "{elapsed:?}");
        assert_eq!(
            result.unwrap(),
            (4..=9).map(|pid| (pid, json!(pid))).collect::<Vec<_>>()
        );
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
