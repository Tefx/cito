//! Local HTTP framing, MCP transport, and peer-process attribution.
use super::super::mcp::{dispatch_tool, tools_list};
use super::super::ui_ipc::REQUEST_LIMIT;
use super::*;
use crate::plugins::CITO_VERSION;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::AtomicUsize;

struct HttpWork;

static HTTP_WORKERS: AtomicUsize = AtomicUsize::new(0);
const MAX_REQUESTS_PER_CONNECTION: usize = 8;
const PEER_PID_TTL: Duration = Duration::from_secs(1);
// A system-wide lsof scan can exceed 250 ms even under ordinary local load.
const PEER_PID_LOOKUP_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Default)]
struct HttpConnection {
    work: Option<HttpWork>,
    peer_pid: Option<(u32, String, Instant)>,
}

impl HttpConnection {
    fn peer_pid(&mut self, stream: &TcpStream) -> Option<u32> {
        // The accepted stream can outlive its original client's ownership.
        #[cfg(target_os = "macos")]
        if let Some((pid, started, verified)) = &self.peer_pid {
            if verified.elapsed() < PEER_PID_TTL
                && crate::support::process_start(*pid).as_ref() == Some(started)
            {
                #[cfg(test)]
                super::super::tests::observe_http_peer_cache_check();
                if is_sole_http_peer_owner(*pid, stream) {
                    return Some(*pid);
                }
            }
        }
        self.peer_pid = None;
        let pid = http_client_pid(stream)?;
        let started = crate::support::process_start(pid);
        // Ownership may also have changed during the subprocess lookup.
        #[cfg(target_os = "macos")]
        if !is_sole_http_peer_owner(pid, stream) {
            return None;
        }
        self.peer_pid = started.map(|started| (pid, started, Instant::now()));
        Some(pid)
    }
}

impl HttpWork {
    fn try_start() -> Option<Self> {
        let mut n = HTTP_WORKERS.load(Ordering::Relaxed);
        while n < 64 {
            match HTTP_WORKERS.compare_exchange_weak(n, n + 1, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return Some(Self),
                Err(actual) => n = actual,
            }
        }
        None
    }
}

impl Drop for HttpWork {
    fn drop(&mut self) {
        HTTP_WORKERS.fetch_sub(1, Ordering::Relaxed);
    }
}

fn respond_bytes(stream: &mut TcpStream, status: u16, body: &[u8]) {
    let _ = crate::core::http_boundary::write_json(stream, status, body);
}

pub(in crate::core::daemon) fn respond(stream: &mut TcpStream, status: u16, value: &Value) {
    if let Ok(body) = serde_json::to_vec(value) {
        respond_bytes(stream, status, &body);
    }
}

#[cfg(target_os = "macos")]
fn is_sole_http_peer_owner(pid: u32, stream: &TcpStream) -> bool {
    use std::net::SocketAddr::V4;

    // The daemon listener and the fallback resolver are both IPv4 loopback only.
    let (Ok(V4(client)), Ok(V4(server))) = (stream.peer_addr(), stream.local_addr()) else {
        return false;
    };
    let (Ok(pid), Ok(daemon)) = (i32::try_from(pid), i32::try_from(std::process::id())) else {
        return false;
    };
    // SAFETY: a null buffer and zero length query the PID count without writing.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 || pid <= 0 || pid == daemon {
        return false;
    }
    // Leave room for new processes between the size query and the snapshot.
    let mut pids = vec![0i32; count as usize + 128];
    let Ok(bytes) = i32::try_from(std::mem::size_of_val(pids.as_slice())) else {
        return false;
    };
    // SAFETY: pids is aligned and writable for bytes bytes; libproc borrows it
    // synchronously and does not retain the pointer.
    let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    if count <= 0 || count as usize >= pids.len() {
        return false;
    }
    let pids = &pids[..count as usize];
    pids.contains(&pid)
        && pids
            .iter()
            .copied()
            .filter(|&other| other > 0 && other != pid && other != daemon)
            .all(|other| owns_http_peer_socket(other, client, server) == Some(false))
        && owns_http_peer_socket(pid, client, server) == Some(true)
}

#[cfg(target_os = "macos")]
fn owns_http_peer_socket(
    pid: i32,
    client: std::net::SocketAddrV4,
    server: std::net::SocketAddrV4,
) -> Option<bool> {
    let fd_size = std::mem::size_of::<libc::proc_fdinfo>();
    // SAFETY: with a null buffer and zero length, PROC_PIDLISTFDS returns the
    // required capacity without writing to caller memory.
    let capacity =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if capacity <= 0 {
        // Like unprivileged lsof, libproc cannot see other users' descriptors.
        return Some(false);
    }
    if !(capacity as usize).is_multiple_of(fd_size) {
        return None;
    }
    let mut fds = vec![
        libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0
        };
        capacity as usize / fd_size
    ];
    // SAFETY: fds is an aligned, initialized buffer writable for capacity bytes;
    // the call only borrows it for this synchronous read.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr().cast(),
            capacity,
        )
    };
    if written <= 0 {
        return Some(false);
    }
    if written >= capacity || !(written as usize).is_multiple_of(fd_size) {
        // A saturated or malformed table is not evidence of exclusive ownership.
        return None;
    }
    Some(fds[..written as usize / fd_size].iter().any(|fd| {
        if fd.proc_fdtype != libc::PROX_FDTYPE_SOCKET as u32 {
            return false;
        }
        // libc lacks this flavor and layout. These are the LP64 socket_fdinfo
        // size, alignment, and field offsets from Darwin's sys/proc_info.h.
        const PROC_PIDFDSOCKETINFO: i32 = 3;
        const SOCKINFO_TCP: i32 = 2;
        const TSI_S_ESTABLISHED: i32 = 4;
        const SOCKET_FDINFO_SIZE: usize = 792;
        #[repr(C, align(8))]
        struct SocketFdInfo([u8; SOCKET_FDINFO_SIZE]);
        let mut info = SocketFdInfo([0; SOCKET_FDINFO_SIZE]);
        let size = std::mem::size_of_val(&info) as i32;
        // SAFETY: info is aligned as socket_fdinfo and writable for exactly size
        // bytes; the call borrows it synchronously and does not retain the pointer.
        if unsafe {
            libc::proc_pidfdinfo(
                pid,
                fd.proc_fd,
                PROC_PIDFDSOCKETINFO,
                info.0.as_mut_ptr().cast(),
                size,
            )
        } != size
        {
            return false;
        }
        let word = |offset| i32::from_ne_bytes(std::array::from_fn(|i| info.0[offset + i]));
        // The client socket has the reverse of the accepted stream's local/peer tuple.
        word(184) == libc::AF_INET // psi.soi_family
            && word(256) == SOCKINFO_TCP // psi.soi_kind, selecting pri_tcp
            && word(344) == TSI_S_ESTABLISHED // tcpsi_state
            && word(264) == i32::from(server.port().to_be()) // insi_fport
            && word(268) == i32::from(client.port().to_be()) // insi_lport
            && info.0[308..312] == server.ip().octets() // insi_faddr.ina_46.i46a_addr4
            && info.0[324..328] == client.ip().octets() // insi_laddr.ina_46.i46a_addr4
    }))
}

fn http_client_pid(stream: &TcpStream) -> Option<u32> {
    let remote = stream.peer_addr().ok()?.port();
    let local = stream.local_addr().ok()?.port();
    let mut command = Command::new("/usr/sbin/lsof");
    command
        .args([
            "-nP",
            &format!("-iTCP:{remote}"),
            "-sTCP:ESTABLISHED",
            "-Fpn",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(test)]
    super::super::tests::observe_http_peer_lookup();
    let output =
        crate::support::run_command_with_timeout(command, PEER_PID_LOOKUP_TIMEOUT).ok()??;
    let expected = format!("n127.0.0.1:{remote}->127.0.0.1:{local}");
    let mut pid = None;
    let mut matches = std::collections::HashSet::new();
    for line in std::str::from_utf8(&output.stdout).ok()?.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse::<u32>().ok();
        }
        if line == expected {
            if let Some(pid) = pid {
                matches.insert(pid);
            }
        }
    }
    matches.remove(&std::process::id());
    if matches.len() == 1 {
        matches.into_iter().next()
    } else {
        None
    }
}

pub(in crate::core::daemon) fn handle_http(
    mut stream: TcpStream,
    service: Arc<Mutex<DaemonService>>,
    health: Arc<Value>,
) {
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let Ok(copy) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(copy);
    let mut connection = HttpConnection::default();
    for remaining in (0..MAX_REQUESTS_PER_CONNECTION).rev() {
        if !handle_http_request(
            &mut stream,
            &mut reader,
            &service,
            &health,
            &mut connection,
            remaining > 0,
        ) {
            // No more requests will run on this socket. The listener still
            // counts the connection while it drains, but the work slot is free.
            drop(connection);
            crate::core::http_boundary::finish_http(&mut stream);
            return;
        }
        // Keep the work slot while idle, leaving the listener's health reserve
        // available. A pooled socket that goes idle closes without a response.
        if stream.set_read_timeout(Some(PEER_PID_TTL)).is_err()
            || !matches!(reader.fill_buf(), Ok(bytes) if !bytes.is_empty())
        {
            break;
        }
    }
}

fn handle_http_request(
    stream: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    service: &Arc<Mutex<DaemonService>>,
    health: &Value,
    connection: &mut HttpConnection,
    can_reuse: bool,
) -> bool {
    let head = match crate::core::http_boundary::read_head(reader, 16_384) {
        Ok(head) => head,
        Err((status, error)) => {
            respond(stream, status, &json!({"error":error}));
            return false;
        }
    };
    let method = head.method.as_str();
    let path = head.target.split('?').next().unwrap_or("");
    let length = head.length;
    let can_reuse = can_reuse
        && !head.headers.get("connection").is_some_and(|value| {
            value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("close"))
        });
    let headers: Map<String, Value> = head
        .headers
        .into_iter()
        .map(|(key, value)| (key, json!(value)))
        .collect();
    let Ok(bound) = stream.local_addr() else {
        return false;
    };
    if let Err((status, error)) = crate::core::http_boundary::validate_hub(
        bound,
        method,
        headers.get("host").and_then(Value::as_str),
        headers.get("origin").and_then(Value::as_str),
        headers.get("content-type").and_then(Value::as_str),
        false,
    ) {
        respond(stream, status, &json!({"error":error}));
        return false;
    }
    if path == "/health" && method == "GET" {
        if shutdown_requested() {
            respond(stream, 503, &json!({"ok":false,"error":"Shutting down"}));
        } else {
            respond(stream, 200, health);
        }
        return false;
    }
    // Classify validated health requests before acquiring a work slot.
    if connection.work.is_none() {
        let Some(work) = HttpWork::try_start() else {
            respond(stream, 503, &json!({"error":"Too many daemon requests"}));
            return false;
        };
        connection.work = Some(work);
    }
    if method != "POST" {
        respond(stream, 404, &json!({"error":"Not found"}));
        return false;
    }
    if length > REQUEST_LIMIT {
        respond(stream, 413, &json!({"error":"Request body exceeds 1 MiB"}));
        return false;
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        respond(stream, 400, &json!({"error":"Incomplete request body"}));
        return false;
    }
    let Ok(mut parsed) = serde_json::from_slice::<Value>(&body) else {
        let error = if path == "/mcp" {
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}})
        } else {
            json!({"error":"Invalid JSON"})
        };
        respond(stream, 400, &error);
        return false;
    };
    if matches!(path, "/ui" | "/action" | "/control") {
        match dispatch_action(service, &parsed, None) {
            Ok(v) => respond(stream, 200, &v),
            Err(e) => respond(stream, 400, &json!({"ok":false,"error":e})),
        }
        return false;
    }
    let Ok(s) = service.lock() else {
        return false;
    };
    if s.closing || SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
        respond(stream, 503, &json!({"error":"Shutting down"}));
        return false;
    }
    drop(s);
    if path != "/mcp" {
        respond(stream, 404, &json!({"error":"Not found"}));
        return false;
    }
    let id = parsed["id"].clone();
    if !parsed.is_object() || parsed["jsonrpc"] != "2.0" || !parsed["method"].is_string() {
        respond(
            stream,
            400,
            &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32600,"message":"Invalid Request"}}),
        );
        return false;
    }
    let method = string(&parsed, "method").to_string();
    if parsed.get("id").is_none() {
        respond(stream, 202, &Value::Null);
        return false;
    }
    let mut reuse = false;
    let result = match method.as_str() {
        "initialize" => {
            json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"cito","version":CITO_VERSION}})
        }
        "tools/list" => tools_list(),
        "ping" => json!({}),
        "tools/call" => {
            if !parsed["params"].is_object() {
                parsed["params"] = json!({});
            }
            let name = string(&parsed["params"], "name").to_string();
            if !parsed["params"]["_meta"].is_object() {
                parsed["params"]["_meta"] = json!({});
            }
            if let Some(meta) = parsed["params"]["_meta"].as_object_mut() {
                meta.remove("_http_client");
                meta.insert("headers".into(), Value::Object(headers));
                // The explicit PID takes precedence; only attribute the peer
                // socket when resolution would actually use it.
                if name != "cito_find"
                    && matches!(
                        crate::support::caller_pid_hint(&Value::Object(meta.clone()), true),
                        Ok(None)
                    )
                {
                    if let Some(pid) = connection.peer_pid(stream) {
                        meta.insert("_http_client".into(), json!(pid));
                        reuse = can_reuse && connection.peer_pid.is_some();
                    }
                }
                #[cfg(test)]
                super::super::tests::observe_http_attribution(meta.get("_http_client"));
            }
            let args = parsed["params"]
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match dispatch_tool(service, &name, &args, &parsed["params"]["_meta"]) {
                Ok(payload) => {
                    json!({"isError":false,"content":[{"type":"text","text":payload.to_string()}]})
                }
                Err(error) => {
                    let (code, message) = crate::core::http_boundary::coded_error(&error);
                    json!({"isError":true,"content":[{"type":"text","text":json!({"ok":false,"error":{"code":code,"message":message.trim(),"retryable":matches!(code,"HUB_UNAVAILABLE"|"UNAVAILABLE"|"INTERNAL_ERROR"|"AUTH_UNAVAILABLE"|"STALE_GENERATION")}}).to_string()}]})
                }
            }
        }
        _ => {
            respond(
                stream,
                200,
                &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}),
            );
            return false;
        }
    };
    let response = json!({"jsonrpc":"2.0","id":id,"result":result});
    if reuse {
        let body = response.to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: keep-alive\r\nContent-Length: {}\r\n\r\n{body}", body.len()).is_ok()
    } else {
        respond(stream, 200, &response);
        false
    }
}
