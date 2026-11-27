use super::hub::HubEngine;
use super::identity::{principal, string};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;

fn hub_error(code: &str, message: &str) -> String {
    if message.starts_with(&format!("{code}:")) {
        message.into()
    } else {
        format!("{code}: {message}")
    }
}

fn authorization(bearer: Option<&str>) -> io::Result<Option<String>> {
    bearer
        .map(|token| {
            if super::config::valid_hub_token(token) {
                Ok(format!("Bearer {token}"))
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Invalid Hub bearer token",
                ))
            }
        })
        .transpose()
}

const IO_SLICE: Duration = Duration::from_millis(50);
const HEADER_LIMIT: u64 = 65_536;
pub(super) const RPC_BODY_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) struct HttpDeadline<'a> {
    pub at: Instant,
    pub connect_at: Option<Instant>,
    pub cancelled: Option<&'a dyn Fn() -> bool>,
}

impl HttpDeadline<'_> {
    pub(super) fn uncancelled(at: Instant) -> Self {
        Self {
            at,
            connect_at: None,
            cancelled: None,
        }
    }

    fn check(self) -> io::Result<()> {
        check_cancelled(self.cancelled)?;
        if Instant::now() >= self.at {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }
}

fn check_cancelled(cancelled: Option<&dyn Fn() -> bool>) -> io::Result<()> {
    if cancelled.is_some_and(|check| check()) {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "HTTP request cancelled",
        ));
    }
    Ok(())
}

fn resolve(url: &url::Url, deadline: HttpDeadline<'_>) -> io::Result<Vec<SocketAddr>> {
    deadline.check()?;
    let address = |ip| {
        Ok(vec![SocketAddr::new(
            ip,
            url.port_or_known_default()
                .ok_or(io::ErrorKind::InvalidInput)?,
        )])
    };
    match url.host() {
        Some(url::Host::Ipv4(ip)) => return address(ip.into()),
        Some(url::Host::Ipv6(ip)) => return address(ip.into()),
        Some(url::Host::Domain(_)) => {}
        None => return Err(io::ErrorKind::InvalidInput.into()),
    }

    // The system resolver can block inside getaddrinfo. Bound outstanding lookups
    // without queuing work that would outlive the caller's deadline.
    static RESOLVERS: AtomicUsize = AtomicUsize::new(0);
    struct Reservation;
    impl Drop for Reservation {
        fn drop(&mut self) {
            RESOLVERS.fetch_sub(1, Ordering::Relaxed);
        }
    }
    RESOLVERS
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
            (n < 16).then_some(n + 1)
        })
        .map_err(|_| io::Error::other("HTTP resolver busy"))?;
    let reservation = Reservation;
    let url = url.clone();
    let (send, receive) = mpsc::sync_channel(1);
    thread::Builder::new().spawn(move || {
        let _reservation = reservation;
        let _ = send.send(url.socket_addrs(|| None));
    })?;
    loop {
        deadline.check()?;
        let slice = deadline
            .at
            .saturating_duration_since(Instant::now())
            .min(IO_SLICE);
        match receive.recv_timeout(slice) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("HTTP resolver stopped"))
            }
        }
    }
}

fn socket_ready(fd: RawFd, events: i16, until: Instant) -> io::Result<bool> {
    crate::support::poll_until(
        &mut [libc::pollfd {
            fd,
            events,
            revents: 0,
        }],
        Some(until),
    )
    .map_err(io::Error::other)
}

fn connect(address: SocketAddr, deadline: HttpDeadline<'_>) -> io::Result<TcpStream> {
    deadline.check()?;
    if deadline.cancelled.is_none() {
        let now = Instant::now();
        if now >= deadline.at {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let socket = TcpStream::connect_timeout(&address, deadline.at - now)?;
        socket.set_nonblocking(true)?;
        deadline.check()?;
        return Ok(socket);
    }
    let domain = if address.is_ipv4() {
        libc::AF_INET
    } else {
        libc::AF_INET6
    };
    let kind = libc::SOCK_STREAM;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let kind = kind | libc::SOCK_CLOEXEC;
    // SAFETY: only scalar socket parameters are passed; on success the new fd is owned here.
    let fd = unsafe { libc::socket(domain, kind, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a new and exclusively owned TCP socket.
    let socket = unsafe { TcpStream::from_raw_fd(fd) };
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    // SAFETY: socket owns fd; F_SETFD takes an integer argument and retains ownership.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    #[cfg(target_vendor = "apple")]
    {
        let on: libc::c_int = 1;
        // SAFETY: socket owns fd; on is readable for the exact option length.
        if unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                (&on as *const libc::c_int).cast(),
                std::mem::size_of_val(&on) as libc::socklen_t,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    socket.set_nonblocking(true)?;
    deadline.check()?;
    let initiation = |ptr: *const libc::sockaddr, len: libc::socklen_t| {
        // SAFETY: each caller supplies a live, initialized sockaddr of the matching family and length;
        // socket owns fd and connect does not retain the pointer or take ownership.
        if unsafe { libc::connect(fd, ptr, len) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    };
    let result = match address {
        SocketAddr::V4(address) => {
            // SAFETY: zero is valid for every integer field and initializes platform-specific padding.
            let mut raw: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            raw.sin_family = libc::AF_INET as libc::sa_family_t;
            raw.sin_port = address.port().to_be();
            raw.sin_addr.s_addr = u32::from_ne_bytes(address.ip().octets());
            let len = std::mem::size_of_val(&raw) as libc::socklen_t;
            #[cfg(target_vendor = "apple")]
            {
                raw.sin_len = len as u8;
            }
            initiation((&raw as *const libc::sockaddr_in).cast(), len)
        }
        SocketAddr::V6(address) => {
            // SAFETY: zero is valid for every integer field and initializes platform-specific padding.
            let mut raw: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
            raw.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            raw.sin6_port = address.port().to_be();
            raw.sin6_flowinfo = address.flowinfo().to_be();
            raw.sin6_scope_id = address.scope_id();
            raw.sin6_addr.s6_addr = address.ip().octets();
            let len = std::mem::size_of_val(&raw) as libc::socklen_t;
            #[cfg(target_vendor = "apple")]
            {
                raw.sin6_len = len as u8;
            }
            initiation((&raw as *const libc::sockaddr_in6).cast(), len)
        }
    };
    match result {
        Ok(()) => {}
        Err(e)
            if e.raw_os_error() == Some(libc::EINPROGRESS)
                || e.kind() == io::ErrorKind::Interrupted =>
        {
            loop {
                deadline.check()?;
                if socket_ready(
                    socket.as_raw_fd(),
                    libc::POLLOUT,
                    deadline.at.min(Instant::now() + IO_SLICE),
                )? {
                    if let Some(error) = socket.take_error()? {
                        return Err(error);
                    }
                    break;
                }
            }
        }
        Err(e) => return Err(e),
    }
    deadline.check()?;
    Ok(socket)
}

fn connect_http(url: &url::Url, mut deadline: HttpDeadline<'_>) -> io::Result<TcpStream> {
    if let Some(connect_at) = deadline.connect_at {
        deadline.at = deadline.at.min(connect_at);
    }
    let addresses = resolve(url, deadline)?;
    let attempt = Duration::from_millis(if addresses.len() == 1 { 1500 } else { 500 });
    let mut error = io::Error::new(io::ErrorKind::NotConnected, "HTTP address unavailable");
    for address in addresses {
        deadline.check()?;
        match connect(
            address,
            HttpDeadline {
                at: deadline.at.min(Instant::now() + attempt),
                ..deadline
            },
        ) {
            Ok(socket) => return Ok(socket),
            Err(e) => error = e,
        }
    }
    deadline.check()?;
    Err(error)
}

pub(super) static HTTP_TLS: LazyLock<Arc<rustls::ClientConfig>> = LazyLock::new(|| {
    Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::from_iter(
                webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
            ))
            .with_no_client_auth(),
    )
});

struct SocketIo<'a> {
    socket: TcpStream,
    deadline: HttpDeadline<'a>,
    write_gate: Option<&'a Mutex<()>>,
    streaming: bool,
}

impl SocketIo<'_> {
    fn check(&self) -> io::Result<()> {
        if self.streaming {
            check_cancelled(self.deadline.cancelled)
        } else {
            self.deadline.check()
        }
    }

    fn wait(&self, events: i16) -> io::Result<()> {
        let slice = Instant::now() + IO_SLICE;
        let until = if self.streaming {
            slice
        } else {
            slice.min(self.deadline.at)
        };
        let ready = socket_ready(self.socket.as_raw_fd(), events, until)?;
        self.check()?;
        if self.streaming && !ready {
            // Let the SSE consumer apply its own idle policy after the bounded prelude.
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }
}

impl Read for SocketIo<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            self.check()?;
            match self.socket.read(bytes) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => self.wait(libc::POLLIN)?,
                result => return result,
            }
        }
    }
}

impl Write for SocketIo<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            let result = {
                // Slot close and the actual nonblocking write share this fence, including TLS writes.
                let _guard = self
                    .write_gate
                    .map(|gate| gate.lock())
                    .transpose()
                    .map_err(|_| io::Error::other("HTTP write gate poisoned"))?;
                self.check()?;
                self.socket.write(bytes)
            };
            match result {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => self.wait(libc::POLLOUT)?,
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.check()
    }
}

trait HttpIo: Read + Write {
    fn start_streaming(&mut self);
}
impl HttpIo for SocketIo<'_> {
    fn start_streaming(&mut self) {
        self.streaming = true;
    }
}
impl HttpIo for rustls::StreamOwned<rustls::ClientConnection, SocketIo<'_>> {
    fn start_streaming(&mut self) {
        self.sock.streaming = true;
    }
}

pub(super) struct HttpResponse<R> {
    pub status: u16,
    body: ResponseBody<R>,
}

impl<R: Read> HttpResponse<R> {
    pub(super) fn read_body(mut self, limit: u64) -> io::Result<Vec<u8>> {
        let too_large =
            || io::Error::new(io::ErrorKind::FileTooLarge, "HTTP body exceeds frame limit");
        if matches!(self.body.framing, Framing::Length(n) if n > limit) {
            return Err(too_large());
        }
        let mut bytes = Vec::new();
        self.body.by_ref().take(limit + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(too_large());
        }
        Ok(bytes)
    }
}

struct ResponseBody<R> {
    reader: BufReader<R>,
    framing: Framing,
}

enum Framing {
    Length(u64),
    CloseDelimited,
    Chunked(Chunks),
}

#[derive(Default)]
struct Chunks {
    remaining: u64,
    terminator: bool,
    trailers: Option<u64>,
    line: Vec<u8>,
}

impl Chunks {
    // Preserve partial framing across SSE idle timeouts. The trailer budget
    // bounds metadata after the last chunk separately from decoded body bytes.
    fn read(&mut self, reader: &mut impl BufRead, out: &mut [u8]) -> io::Result<usize> {
        while self.remaining == 0 {
            let budget = self.trailers.unwrap_or(8192);
            reader
                .by_ref()
                .take(budget - self.line.len() as u64)
                .read_until(b'\n', &mut self.line)?;
            let line = self
                .line
                .strip_suffix(b"\n")
                .ok_or(io::ErrorKind::InvalidData)?;
            if let Some(left) = &mut self.trailers {
                *left -= self.line.len() as u64;
                if line.is_empty() || line == b"\r" {
                    return Ok(0);
                }
            } else if self.terminator {
                if line != b"\r" {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                self.terminator = false;
            } else {
                let digits = line.split(|b| *b == b';').next().unwrap_or_default();
                self.remaining = std::str::from_utf8(digits)
                    .ok()
                    .and_then(|s| u64::from_str_radix(s.trim(), 16).ok())
                    .ok_or(io::ErrorKind::InvalidData)?;
                if self.remaining == 0 {
                    self.trailers = Some(HEADER_LIMIT);
                }
            }
            self.line.clear();
        }
        let n = read_exact_fragment(reader, out, &mut self.remaining)?;
        self.terminator = self.remaining == 0;
        Ok(n)
    }
}

fn read_exact_fragment(
    reader: &mut impl Read,
    out: &mut [u8],
    remaining: &mut u64,
) -> io::Result<usize> {
    let limit = (*remaining).min(out.len() as u64) as usize;
    if limit == 0 {
        return Ok(0);
    }
    let n = reader.read(&mut out[..limit])?;
    if n == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    *remaining -= n as u64;
    Ok(n)
}

impl<R: Read> Read for ResponseBody<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        match &mut self.framing {
            Framing::Length(left) => read_exact_fragment(&mut self.reader, out, left),
            Framing::CloseDelimited => self.reader.read(out),
            Framing::Chunked(chunks) => {
                let n = chunks.read(&mut self.reader, out)?;
                if n == 0 {
                    self.framing = Framing::Length(0);
                }
                Ok(n)
            }
        }
    }
}

pub(super) fn read_response<R: Read>(stream: R) -> io::Result<HttpResponse<R>> {
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidData, message);
    let mut reader = BufReader::new(stream);
    let mut headers = reader.by_ref().take(HEADER_LIMIT);
    let mut line = String::new();
    if headers.read_line(&mut line)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            super::http_boundary::PEER_CLOSED,
        ));
    }
    let truncated = || invalid("Truncated or oversized HTTP headers");
    if !line.ends_with('\n') {
        return Err(truncated());
    }
    let mut parts = line.split_whitespace();
    if !matches!(parts.next(), Some("HTTP/1.0" | "HTTP/1.1")) {
        return Err(invalid("Invalid HTTP version"));
    }
    let status = parts
        .next()
        .filter(|s| s.len() == 3)
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|s| (100..600).contains(s))
        .ok_or_else(|| invalid("Invalid HTTP status"))?;
    let mut length = None;
    let mut chunked = false;
    loop {
        line.clear();
        headers.read_line(&mut line)?;
        if !line.ends_with('\n') {
            return Err(truncated());
        }
        if matches!(line.as_str(), "\r\n" | "\n") {
            break;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| invalid("Invalid HTTP header"))?;
        if name.eq_ignore_ascii_case("content-length") {
            let n = value
                .trim()
                .parse::<u64>()
                .map_err(|_| invalid("Invalid Content-Length"))?;
            if length.replace(n).is_some() {
                return Err(invalid("Duplicate Content-Length"));
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.trim().eq_ignore_ascii_case("chunked") {
                return Err(invalid("Invalid Transfer-Encoding"));
            }
            chunked = true;
        }
    }
    let framing = match (chunked, length, status) {
        (true, Some(_), _) => return Err(invalid("Conflicting HTTP body framing")),
        (_, _, 100..=199 | 204 | 304) => Framing::Length(0),
        (true, None, _) => Framing::Chunked(Chunks::default()),
        (false, Some(n), _) => Framing::Length(n),
        (false, None, _) => Framing::CloseDelimited,
    };
    Ok(HttpResponse {
        status,
        body: ResponseBody { reader, framing },
    })
}

fn exchange<'a>(
    url: &url::Url,
    method: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    deadline: HttpDeadline<'a>,
    tls: &Arc<rustls::ClientConfig>,
    write_gate: Option<&'a Mutex<()>>,
) -> io::Result<HttpResponse<Box<dyn HttpIo + 'a>>> {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let authority = &url[url::Position::BeforeHost..url::Position::AfterPort];
    let target = &url[url::Position::BeforePath..url::Position::AfterQuery];
    let mut request = format!("{method} {target} HTTP/1.1\r\nHost: {authority}\r\nAccept-Encoding: identity\r\nConnection: close\r\n");
    for (key, value) in headers {
        if value.bytes().any(|b| b.is_ascii_control()) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        use std::fmt::Write as _;
        let _ = write!(request, "{key}: {value}\r\n");
    }
    if let Some(body) = body {
        use std::fmt::Write as _;
        let _ = write!(request, "Content-Length: {}\r\n", body.len());
    }
    request.push_str("\r\n");
    let mut socket = SocketIo {
        socket: connect_http(url, deadline)?,
        deadline,
        write_gate,
        streaming: false,
    };
    let mut stream: Box<dyn HttpIo> = if url.scheme() == "https" {
        // URL hosts bracket IPv6 literals; rustls expects the unbracketed IP.
        let host = url
            .host_str()
            .ok_or(io::ErrorKind::InvalidInput)?
            .trim_matches(['[', ']']);
        let name =
            rustls::pki_types::ServerName::try_from(host.to_owned()).map_err(io::Error::other)?;
        let mut connection =
            rustls::ClientConnection::new(tls.clone(), name).map_err(io::Error::other)?;
        connection.complete_io(&mut socket)?;
        Box::new(rustls::StreamOwned::new(connection, socket))
    } else {
        Box::new(socket)
    };
    deadline.check()?;
    stream.write_all(request.as_bytes())?;
    if let Some(body) = body {
        stream.write_all(body)?;
    }
    stream.flush()?;
    let response = read_response(stream)?;
    deadline.check()?;
    Ok(response)
}

pub(super) fn http_request<'a>(
    url: &url::Url,
    method: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    deadline: HttpDeadline<'a>,
    tls: &Arc<rustls::ClientConfig>,
) -> io::Result<HttpResponse<impl Read + 'a>> {
    exchange(url, method, headers, body, deadline, tls, None)
}

#[derive(Clone)]
pub struct HubClient {
    pub name: String,
    pub config: Value,
    pub identity: Value,
    pub bearer: Option<String>,
    pub expires_at: u64,
    pub auth_retry_at: u64,
    pub rejected_bearer: Arc<Mutex<Option<String>>>,
    directory_cache: Arc<Mutex<Vec<Value>>>,
    directory_revision: Arc<AtomicU64>,
    // Serialize network writers without holding the cache lock used by UI projections.
    directory_updates: Arc<Mutex<()>>,
    server_clock: Arc<Mutex<Option<(u64, Instant)>>>,
    pub local: Option<Arc<HubEngine>>,
    // RPC reachability is independent of the optional event-stream hint.
    pub status: Arc<Mutex<String>>,
    stream_live: Arc<AtomicBool>,
    event_revision: Arc<AtomicU64>,
    event_wait: Arc<(Mutex<()>, std::sync::Condvar)>,
    local_event_wake: Arc<Mutex<Option<Arc<crate::support::WakePipe>>>>,
    directory_observed_at: Arc<Mutex<Option<Instant>>>,
    human_observed_at: Arc<Mutex<Option<Instant>>>,
    event_credentials: Arc<Mutex<Option<String>>>,
    pub closed: Arc<AtomicBool>,
    pub(super) commit_gate: Arc<Mutex<()>>,
    tls: Arc<rustls::ClientConfig>,
}
impl HubClient {
    pub fn new(name: String, config: Value, user: &str, local: Option<Arc<HubEngine>>) -> Self {
        let bearer = config["token"]
            .as_str()
            .filter(|token| config["authMode"] != "none" && super::config::valid_hub_token(token))
            .map(str::to_owned);
        let status = if local.is_some() {
            "connected"
        } else {
            "connecting"
        };
        Self {
            name,
            config,
            identity: principal(user),
            event_credentials: Arc::new(Mutex::new(bearer.clone())),
            bearer,
            expires_at: u64::MAX,
            auth_retry_at: 0,
            rejected_bearer: Arc::new(Mutex::new(None)),
            directory_cache: Arc::new(Mutex::new(Vec::new())),
            directory_revision: Arc::new(AtomicU64::new(0)),
            directory_updates: Arc::new(Mutex::new(())),
            server_clock: Arc::new(Mutex::new(None)),
            local,
            status: Arc::new(Mutex::new(status.into())),
            stream_live: Arc::new(AtomicBool::new(false)),
            event_revision: Arc::new(AtomicU64::new(1)),
            event_wait: Arc::new((Mutex::new(()), std::sync::Condvar::new())),
            local_event_wake: Arc::new(Mutex::new(None)),
            directory_observed_at: Arc::new(Mutex::new(None)),
            human_observed_at: Arc::new(Mutex::new(None)),
            closed: Arc::new(AtomicBool::new(false)),
            commit_gate: Arc::default(),
            tls: HTTP_TLS.clone(),
        }
    }
    pub fn rpc(&self, op: &str, params: &Value) -> Result<Value, String> {
        if self.closed.load(Ordering::SeqCst) {
            return Err("HUB_UNAVAILABLE: Hub disconnected".into());
        }
        if let Some(local) = &self.local {
            return local.rpc(op, params, Some(&self.identity));
        }
        let bearer = self
            .event_credentials
            .lock()
            .map_err(|_| "STATE_ERROR: Credentials lock poisoned")?
            .clone();
        let auth = authorization(bearer.as_deref()).map_err(|e| format!("INVALID_INPUT: {e}"))?;
        let url = url::Url::parse(&format!(
            "{}/v1/rpc",
            string(&self.config, "url").trim_end_matches('/')
        ))
        .map_err(|e| self.disconnected(e))?;
        let mut headers = vec![
            ("Content-Type", "application/json"),
            ("X-Cito-User", string(&self.identity, "owner_user")),
        ];
        if let Some(value) = auth.as_deref() {
            headers.push(("Authorization", value));
        }
        let cancelled = || self.request_cancelled();
        let deadline = HttpDeadline {
            at: Instant::now() + Duration::from_secs(if url.scheme() == "http" { 10 } else { 5 }),
            connect_at: None,
            cancelled: Some(&cancelled),
        };
        let body = json!({"op":op,"params":params}).to_string();
        let response = exchange(
            &url,
            "POST",
            &headers,
            Some(body.as_bytes()),
            deadline,
            &self.tls,
            Some(&self.event_wait.0),
        )
        .map_err(|e| self.disconnected(e))?;
        let status = response.status;
        let bytes = if status == 202 && url.scheme() == "http" {
            Vec::new()
        } else {
            response
                .read_body(RPC_BODY_LIMIT)
                .map_err(|e| self.disconnected(e))?
        };
        deadline.check().map_err(|e| self.disconnected(e))?;
        self.parse_rpc_response(status, &bytes, &bearer)
    }
    fn request_cancelled(&self) -> bool {
        super::daemon::shutdown_requested() || self.closed.load(Ordering::SeqCst)
    }
    fn disconnected(&self, error: impl std::fmt::Display) -> String {
        if let Ok(mut status) = self.status.lock() {
            *status = "disconnected".into();
        }
        format!("HUB_UNAVAILABLE: {error}")
    }
    fn parse_rpc_response(
        &self,
        status: u16,
        bytes: &[u8],
        bearer: &Option<String>,
    ) -> Result<Value, String> {
        if status == 401 {
            self.reject_token(bearer);
        }
        if let Ok(mut state) = self.status.lock() {
            *state = "connected".into();
        }
        let response: Value = match serde_json::from_slice(bytes) {
            Ok(value) => value,
            Err(_) if status != 200 => Value::Null,
            Err(_) => return Err("HUB_UNAVAILABLE: Invalid Hub response JSON".into()),
        };
        if status != 200 || response["ok"] == false || !response["error"].is_null() {
            let fallback = match status {
                401 => "UNAUTHORIZED",
                403 => "FORBIDDEN",
                200 | 500..=599 => "HUB_UNAVAILABLE",
                _ => "INVALID_INPUT",
            };
            return Err(hub_error(
                response["error"]["code"].as_str().unwrap_or(fallback),
                response["error"]["message"]
                    .as_str()
                    .or_else(|| response["message"].as_str())
                    .unwrap_or("Hub rejected request"),
            ));
        }
        if response["ok"] != true {
            return Err("HUB_UNAVAILABLE: Invalid Hub RPC envelope".into());
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| "HUB_UNAVAILABLE: Missing Hub RPC result".into())
    }
    fn reject_token(&self, bearer: &Option<String>) {
        // An old in-flight request must not reject a newly verified credential.
        let current = self.event_credentials.lock();
        if current.as_ref().is_ok_and(|token| **token == *bearer) {
            if let Ok(mut rejected) = self.rejected_bearer.lock() {
                *rejected = Some(super::identity::hash(bearer.as_deref().unwrap_or("")));
            }
        }
    }
    /// Preserve the authority-owned observations and running stream on renewal.
    pub fn inherit_authority(&mut self, old: &Self) {
        self.directory_cache = old.directory_cache.clone();
        self.directory_revision = old.directory_revision.clone();
        self.directory_updates = old.directory_updates.clone();
        self.server_clock = old.server_clock.clone();
        self.status = old.status.clone();
        self.stream_live = old.stream_live.clone();
        self.event_revision = old.event_revision.clone();
        self.event_wait = old.event_wait.clone();
        self.local_event_wake = old.local_event_wake.clone();
        self.rejected_bearer = old.rejected_bearer.clone();
        self.directory_observed_at = old.directory_observed_at.clone();
        self.human_observed_at = old.human_observed_at.clone();
        self.closed = old.closed.clone();
        self.commit_gate = old.commit_gate.clone();
        self.event_credentials = old.event_credentials.clone();
        if let Ok(mut token) = self.event_credentials.lock() {
            *token = self.bearer.clone();
            // Verification of the replacement supersedes any old-token rejection
            // that raced recovery. reject_token uses this same lock.
            if let Ok(mut rejected) = self.rejected_bearer.lock() {
                *rejected = None;
            }
        }
        let _guard = self.event_wait.0.lock().unwrap_or_else(|e| e.into_inner());
        self.event_wait.1.notify_all();
    }
    fn observation_recent(&self, now: Instant) -> bool {
        self.local.is_some()
            || self.directory_observed_at.lock().is_ok_and(|at| {
                at.is_some_and(|at| now.duration_since(at) < Duration::from_secs(60))
            })
    }
    pub(super) fn human_observation_current(&self) -> bool {
        self.auth_valid()
            && (self.local.is_some()
                || self
                    .human_observed_at
                    .lock()
                    .is_ok_and(|at| at.is_some_and(|at| at.elapsed() < Duration::from_secs(60))))
    }
    fn auth_valid(&self) -> bool {
        (self.config["authMode"] != "oidc"
            || (self.bearer.is_some() && self.expires_at > crate::plugins::now_ms() / 1000))
            && self
                .rejected_bearer
                .lock()
                .is_ok_and(|rejected| rejected.is_none())
    }
    pub fn capabilities(&self) -> Value {
        self.capabilities_at(Instant::now())
    }
    fn capabilities_at(&self, now: Instant) -> Value {
        let authenticated = self.auth_valid();
        let rpc = self
            .status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_else(|_| "disconnected".into());
        let stream = self.stream_live.load(Ordering::SeqCst);
        let current = authenticated && self.observation_recent(now);
        let status = if !authenticated {
            "auth_required"
        } else if self.local.is_some() || rpc == "connected" || stream {
            "connected"
        } else {
            &rpc
        };
        json!({"status":status,"auth_valid":authenticated,"rpc_status":rpc,"sse_live":stream,"observation_current":current})
    }
    pub fn connection_status(&self) -> String {
        self.capabilities()["status"]
            .as_str()
            .unwrap_or("disconnected")
            .into()
    }
    pub fn cached_directory(&self) -> Vec<Value> {
        self.directory_cache
            .lock()
            .map(|rows| {
                let mut rows = rows.clone();
                if let Ok(clock) = self.server_clock.lock() {
                    if let Some((now, observed)) = *clock {
                        for row in &mut rows {
                            row["server_now_ms"] = json!(now.saturating_add(
                                observed.elapsed().as_millis().min(u64::MAX as u128) as u64
                            ));
                        }
                    }
                }
                rows
            })
            .unwrap_or_default()
    }
    pub fn server_now(&self) -> u64 {
        self.server_clock
            .lock()
            .ok()
            .and_then(|clock| *clock)
            .map(|(now, observed)| {
                now.saturating_add(observed.elapsed().as_millis().min(u64::MAX as u128) as u64)
            })
            .unwrap_or(0)
    }
    fn observe_server_clock(&self, response: &Value) {
        if let Some(now) = response["server_now_ms"].as_u64() {
            if let Ok(mut clock) = self.server_clock.lock() {
                let previous = clock
                    .map(|(time, observed)| {
                        time.saturating_add(
                            observed.elapsed().as_millis().min(u64::MAX as u128) as u64
                        )
                    })
                    .unwrap_or(0);
                *clock = Some((now.max(previous), Instant::now()));
            }
        }
    }
    pub fn directory_revision(&self) -> u64 {
        self.directory_revision.load(Ordering::Acquire)
    }
    pub fn event_revision(&self) -> u64 {
        self.event_revision.load(Ordering::Acquire)
    }
    fn mutation(&self) {
        {
            let _guard = self.event_wait.0.lock().unwrap_or_else(|e| e.into_inner());
            self.event_revision.fetch_add(1, Ordering::Release);
            self.event_wait.1.notify_all();
        }
        super::daemon::wake_daemon();
    }
    pub fn wait_for_event(&self, revision: u64, deadline: Instant) {
        let guard = self.event_wait.0.lock().unwrap_or_else(|e| e.into_inner());
        let _ = self
            .event_wait
            .1
            .wait_timeout_while(
                guard,
                deadline.saturating_duration_since(Instant::now()),
                |_| self.event_revision() == revision && !self.closed.load(Ordering::SeqCst),
            )
            .unwrap_or_else(|e| e.into_inner());
    }
    pub fn close(&self) {
        if let Ok(_guard) = self.event_wait.0.lock() {
            self.closed.store(true, Ordering::SeqCst);
            self.event_wait.1.notify_all();
            if let Ok(wake) = self.local_event_wake.lock() {
                if let Some(wake) = &*wake {
                    wake.notify();
                }
            }
        }
    }
    fn open_events<'a>(
        &'a self,
        url: &url::Url,
        bearer: Option<&str>,
        tls: &Arc<rustls::ClientConfig>,
        cancelled: &'a dyn Fn() -> bool,
    ) -> io::Result<(u16, impl Read + 'a)> {
        let auth = authorization(bearer)?;
        let mut headers = vec![
            ("Accept", "text/event-stream"),
            ("X-Cito-User", string(&self.identity, "owner_user")),
        ];
        if let Some(value) = auth.as_deref() {
            headers.push(("Authorization", value));
        }
        let deadline = HttpDeadline {
            at: Instant::now() + Duration::from_secs(5),
            connect_at: None,
            cancelled: Some(cancelled),
        };
        let mut response = exchange(
            url,
            "GET",
            &headers,
            None,
            deadline,
            tls,
            Some(&self.event_wait.0),
        )?;
        response.body.reader.get_mut().start_streaming();
        Ok((response.status, response.body))
    }
    pub fn start_events(&self) -> Option<thread::JoinHandle<()>> {
        self.start_events_with(Duration::from_secs(90), self.tls.clone())
    }
    fn start_events_with(
        &self,
        idle_timeout: Duration,
        tls: Arc<rustls::ClientConfig>,
    ) -> Option<thread::JoinHandle<()>> {
        if let Some(local) = &self.local {
            let mut subscription = local.subscribe(Some(self.identity.clone())).ok()?;
            if let Ok(mut wake) = self.local_event_wake.lock() {
                *wake = Some(subscription.wake.clone());
            }
            let client = self.clone();
            client.stream_live.store(true, Ordering::SeqCst);
            return Some(thread::spawn(move || {
                while !client.closed.load(Ordering::SeqCst) {
                    let mut fds = [libc::pollfd {
                        fd: subscription.wake.read_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    }];
                    let _ = crate::support::poll_until(&mut fds, None);
                    subscription.wake.drain();
                    let mut changed = false;
                    let disconnected = loop {
                        match subscription.try_recv() {
                            Ok(_) => changed = true,
                            Err(std::sync::mpsc::TryRecvError::Empty) => break false,
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => break true,
                        }
                    };
                    if changed || disconnected {
                        client.mutation();
                    }
                    if disconnected && !client.closed.load(Ordering::SeqCst) {
                        let Some(local) = &client.local else { break };
                        let Ok(next) = local.subscribe(Some(client.identity.clone())) else {
                            break;
                        };
                        subscription = next;
                        if let Ok(mut wake) = client.local_event_wake.lock() {
                            *wake = Some(subscription.wake.clone());
                            if client.closed.load(Ordering::SeqCst) {
                                subscription.wake.notify();
                            }
                        }
                    }
                }
                client.stream_live.store(false, Ordering::SeqCst);
            }));
        }
        if let Ok(mut token) = self.event_credentials.lock() {
            *token = self.bearer.clone();
        }
        let url = url::Url::parse(&format!(
            "{}/v1/events",
            string(&self.config, "url").trim_end_matches('/')
        ))
        .ok()?;
        let client = self.clone();
        Some(thread::spawn(move || {
            let mut backoff = crate::support::RetryBackoff::default();
            let cancelled = || client.request_cancelled();
            while !client.request_cancelled() {
                let bearer = client
                    .event_credentials
                    .lock()
                    .ok()
                    .and_then(|token| token.clone());
                let response = client.open_events(&url, bearer.as_deref(), &tls, &cancelled);
                let status = response.as_ref().ok().map(|(status, _)| *status);
                if let Some(body) = response
                    .ok()
                    .and_then(|(status, body)| (status == 200).then_some(body))
                {
                    client.stream_live.store(true, Ordering::SeqCst);
                    let mut reader = BufReader::new(body);
                    let connected = Instant::now();
                    let mut last_input = connected;
                    // Only the SSE field name matters. Consume each fragment before
                    // checking cancellation, even for an unterminated data line.
                    let mut prefix = Vec::with_capacity(5);
                    while !client.closed.load(Ordering::SeqCst)
                        && last_input.elapsed() < idle_timeout
                    {
                        let bytes = match reader.fill_buf() {
                            Ok([]) => break,
                            Ok(bytes) => bytes,
                            Err(e)
                                if matches!(
                                    e.kind(),
                                    io::ErrorKind::TimedOut
                                        | io::ErrorKind::WouldBlock
                                        | io::ErrorKind::Interrupted
                                ) =>
                            {
                                continue
                            }
                            Err(_) => break,
                        };
                        last_input = Instant::now();
                        let newline = bytes.iter().position(|b| *b == b'\n');
                        let n = newline.map_or(bytes.len(), |i| i + 1);
                        prefix.extend_from_slice(&bytes[..n.min(5 - prefix.len())]);
                        reader.consume(n);
                        if newline.is_some() {
                            if prefix == b"data:" {
                                backoff.reset();
                                client.mutation();
                            } else if connected.elapsed() >= Duration::from_secs(30) {
                                backoff.reset();
                            }
                            prefix.clear();
                        }
                    }
                }
                client.stream_live.store(false, Ordering::SeqCst);
                if bearer.is_some() && status == Some(401) {
                    client.reject_token(&bearer);
                }
                if matches!(status, Some(401 | 403 | 409)) {
                    let guard = client
                        .event_wait
                        .0
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let _guard = client
                        .event_wait
                        .1
                        .wait_while(guard, |_| {
                            !client.closed.load(Ordering::SeqCst)
                                && client
                                    .event_credentials
                                    .lock()
                                    .is_ok_and(|token| *token == bearer)
                        })
                        .unwrap_or_else(|e| e.into_inner());
                    continue;
                }
                if client.closed.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(guard) = client.event_wait.0.lock() {
                    let _ = client
                        .event_wait
                        .1
                        .wait_timeout_while(guard, backoff.failed(), |_| {
                            !client.closed.load(Ordering::SeqCst)
                        });
                }
            }
        }))
    }
    pub fn human_request(&self, op: &str, params: &Value) -> Result<Value, String> {
        let _update = self
            .directory_updates
            .lock()
            .map_err(|_| "STATE_ERROR: Directory update lock poisoned")?;
        let result = match self.rpc(op, params) {
            Ok(value) => value,
            Err(error) => {
                if let Some(state) = error
                    .strip_prefix("CONTROL_REVISION_CONFLICT: ")
                    .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                {
                    self.cache_human(&state);
                }
                return Err(error);
            }
        };
        if !result["record"].is_object() || !result["record"]["listener_id"].is_string() {
            return Err("HUB_UNAVAILABLE: Invalid Human response".into());
        }
        self.cache_human(&result);
        Ok(result)
    }
    fn cache_human(&self, result: &Value) {
        if !result["record"]["listener_id"].is_string() {
            return;
        }
        self.observe_server_clock(result);
        if let Ok(mut observed) = self.human_observed_at.lock() {
            *observed = Some(Instant::now());
        }
        if result["record"].is_object() {
            let mut record = result["record"].clone();
            record["server_now_ms"] = result["server_now_ms"].clone();
            if let Ok(mut cache) = self.directory_cache.lock() {
                cache.retain(|row| row["listener_id"] != record["listener_id"]);
                cache.push(record);
                self.directory_revision.fetch_add(1, Ordering::Release);
            }
        }
    }
    pub fn kv_get(&self, id: &str) -> Result<Option<(Value, u64)>, String> {
        let value = self.rpc("kv_get", &json!({"key":id}))?;
        self.observe_server_clock(&value);
        if value.is_null() {
            return Ok(None);
        }
        if value["forbidden_scope"] == true {
            return Err("FORBIDDEN_TARGET_SCOPE: Target is private".into());
        }
        let bytes = STANDARD
            .decode(string(&value, "value_b64"))
            .map_err(|e| e.to_string())?;
        let record: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        Ok(Some((record, value["revision"].as_u64().unwrap_or(0))))
    }
    pub fn kv_save(&self, id: &str, value: &Value, revision: Option<u64>) -> Result<Value, String> {
        let mut value = value.clone();
        if let Some(fields) = value.as_object_mut() {
            fields.remove("server_now_ms");
        }
        let data = STANDARD.encode(serde_json::to_vec(&value).map_err(|e| e.to_string())?);
        self.rpc(
            if revision.is_some() {
                "kv_update"
            } else {
                "kv_create"
            },
            &json!({"key":id,"data_b64":data,"revision":revision.unwrap_or(0)}),
        )
    }
    pub fn directory(&self) -> Result<Vec<Value>, String> {
        let _update = self
            .directory_updates
            .lock()
            .map_err(|_| "STATE_ERROR: Directory update lock poisoned")?;
        let mut cursor: Option<String> = None;
        let mut rows = Vec::new();
        loop {
            let raw = self.rpc("kv_snapshot", &json!({"after_key":cursor}))?;
            if !raw["next_cursor"].is_null() && !raw["next_cursor"].is_string() {
                return Err("HUB_UNAVAILABLE: Invalid snapshot cursor".into());
            }
            let items = raw
                .as_array()
                .or_else(|| raw["entries"].as_array())
                .ok_or("HUB_UNAVAILABLE: Missing directory entries")?;
            for item in items {
                if let Some(id) = item["listener_id"].as_str() {
                    if !id.is_empty() {
                        rows.push(item.clone());
                    }
                    continue;
                }
                if raw.is_array() && item.get("value_b64").is_none() {
                    continue;
                }
                let data = STANDARD
                    .decode(
                        item["value_b64"]
                            .as_str()
                            .ok_or("HUB_UNAVAILABLE: Missing directory record")?,
                    )
                    .map_err(|e| e.to_string())?;
                // KV also carries arbitrary bytes and non-registration JSON.
                if let Ok(row) = serde_json::from_slice::<Value>(&data) {
                    if row["listener_id"].as_str().is_some_and(|id| !id.is_empty()) {
                        rows.push(row);
                    }
                }
            }
            let next = raw["next_cursor"].as_str().map(str::to_owned);
            if next.is_some() && next <= cursor {
                return Err("HUB_UNAVAILABLE: Snapshot cursor did not advance".into());
            }
            cursor = next;
            if cursor.is_none() {
                if let Some(now) = raw["server_now_ms"].as_u64() {
                    for row in &mut rows {
                        row["server_now_ms"] = json!(now);
                    }
                }
                self.observe_server_clock(&raw);
                break;
            }
        }
        if let Ok(mut cache) = self.directory_cache.lock() {
            if *cache != rows {
                *cache = rows.clone();
                self.directory_revision.fetch_add(1, Ordering::Release);
            }
        }
        let now = Instant::now();
        if let Ok(mut observed) = self.directory_observed_at.lock() {
            *observed = Some(now);
        }
        if let Ok(mut observed) = self.human_observed_at.lock() {
            *observed = Some(now);
        }
        Ok(rows)
    }
}
