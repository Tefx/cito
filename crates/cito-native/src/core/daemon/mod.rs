use self::channels::ChannelInventory;
use self::http_actions::{handle_http, respond, InteractiveLogin};
use self::ui_ipc::{spawn_ui, UiChild, UiMailbox};
use crate::core::{
    config::{repair_default_hub, ConfigSnapshot},
    hub::HubEngine,
    identity::{listener_id, string},
    receiver::{detach_channel_expected, poll_listener},
    state::LocalState,
    transport::HubClient,
};
use crate::plugins::{now_ms, random_uuid_v4, LoadedPlugins, NativeConfig, CITO_VERSION};
use crate::setup::ensure_cli_shims_native;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::TcpListener;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
mod channels;
#[cfg(test)]
mod event_tests;
pub(crate) mod host_hub;
mod http_actions;
mod mcp;
mod projection;
#[cfg(test)]
mod tests;
mod ui_ipc;
pub use ui_ipc::{bounded_frame, FRAME_LIMIT};

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

static SIGNAL_PIPE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn handle_daemon_sig(_: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
    let fd = SIGNAL_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        // SAFETY: write is async-signal-safe; the nonblocking self-pipe stays open
        // until all handlers are restored at shutdown. No locks or allocations occur.
        unsafe {
            libc::write(fd, b"x".as_ptr().cast(), 1);
        }
    }
}

fn request_shutdown() {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

pub(crate) fn wake_daemon() {
    let fd = SIGNAL_PIPE.load(Ordering::SeqCst);
    if fd >= 0 {
        // SAFETY: the daemon owns this nonblocking descriptor until its workers join.
        unsafe {
            libc::write(fd, [1u8].as_ptr().cast(), 1);
        }
    }
}

pub(crate) fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::SeqCst)
}

fn try_registration(lock: &Mutex<()>) -> Result<Option<std::sync::MutexGuard<'_, ()>>, String> {
    match lock.try_lock() {
        Ok(guard) => Ok(Some(guard)),
        Err(std::sync::TryLockError::WouldBlock) => Ok(None),
        Err(std::sync::TryLockError::Poisoned(_)) => {
            Err("STATE_ERROR: Registration lock poisoned".into())
        }
    }
}

struct OidcRecovery {
    config: Value,
    rejected: Option<String>,
    cancelled: Arc<super::login::Cancellation>,
    worker: thread::JoinHandle<Result<(String, Value), String>>,
}

#[derive(Default)]
struct HubControl {
    login: Option<Arc<InteractiveLogin>>,
    recovery: Option<OidcRecovery>,
    backoff: crate::support::RetryBackoff,
}

struct EndpointProbe {
    due: Instant,
    calm: u32,
    bindings: Vec<String>,
    worker: Option<thread::JoinHandle<Option<Value>>>,
}

type SetupResult = Result<(bool, bool, Vec<String>), String>;

struct SetupInspection {
    generation: u64,
    value: SetupResult,
    done: Arc<AtomicBool>,
    worker: Option<(u64, thread::JoinHandle<SetupResult>)>,
}

impl SetupInspection {
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.worker.is_none() {
            self.done.store(false, Ordering::SeqCst);
        }
    }
}

impl Default for SetupInspection {
    fn default() -> Self {
        Self {
            generation: 0,
            value: Err("Setup inspection pending".into()),
            done: Arc::default(),
            worker: None,
        }
    }
}

use super::session_activity::CompletionNotice;

enum HostHubError {
    Reload(String),
    Action(String),
}
impl HostHubError {
    fn message(&self) -> &str {
        match self {
            Self::Reload(s) | Self::Action(s) => s,
        }
    }
}

/// Immutable request view. Cloned clients share the live authority's close fence;
/// a worker owns no runtime registry, UI state, or background task handles.
#[derive(Clone)]
pub struct HubAccess {
    pub config: NativeConfig,
    pub plugins: LoadedPlugins,
    pub state: LocalState,
    pub slots: HashMap<String, HubClient>,
    host_id: String,
}

impl std::ops::Deref for DaemonService {
    type Target = HubAccess;
    fn deref(&self) -> &HubAccess {
        &self.access
    }
}
impl std::ops::DerefMut for DaemonService {
    fn deref_mut(&mut self) -> &mut HubAccess {
        &mut self.access
    }
}

pub struct DaemonService {
    access: HubAccess,
    pub local: Arc<HubEngine>,
    hub_controls: HashMap<String, HubControl>,
    events: Vec<thread::JoinHandle<()>>,
    receivers: HashMap<String, thread::JoinHandle<()>>,
    caller_cache: Mutex<HashMap<u64, Value>>,
    probes: HashMap<String, EndpointProbe>,
    observations: HashMap<String, (u64, Instant)>,
    session_activity: Option<super::session_activity::SessionActivity>,
    ui: Option<Arc<UiMailbox>>,
    ui_generation: u64,
    last_ui: Instant,
    closing: bool,
    purging: bool,
    host_server: Option<super::hub::HubServer>,
    applied_host_config: Option<Value>,
    uninstall: Option<bool>,
    active_settings_hub: Option<String>,
    menu_open: bool,
    directory_open: bool,
    directory_request: u64,
    last_ui_revision: Mutex<Option<Value>>,
    last_ui_frame: Mutex<Option<Value>>,
    ack_backoff: Arc<Mutex<HashMap<String, (Instant, crate::support::RetryBackoff)>>>,
    setup_serial: Arc<Mutex<()>>,
    retired_auth: Vec<OidcRecovery>,
    setup_inspections: HashMap<String, SetupInspection>,
    setup_completion: Arc<(Mutex<u64>, std::sync::Condvar)>,
    host_hub_error: Option<HostHubError>,
    last_hub_errors: HashMap<String, (String, bool)>,
    channel_inventories: HashMap<String, Arc<ChannelInventory>>,
    activity_filter: String,
    activity_query: String,
}

pub fn run_daemon(config: &NativeConfig, plugins: &LoadedPlugins) {
    if let Err(e) = run_daemon_inner(
        config,
        plugins,
        TcpListener::accept,
        crate::support::poll_until,
    ) {
        eprintln!("cito daemon: {e}");
        std::process::exit(1);
    }
}

fn jwt_exp(token: &str) -> Option<u64> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token.split('.').nth(1)?)
        .ok()?;
    serde_json::from_slice::<Value>(&bytes).ok()?["exp"]
        .as_f64()
        .map(|exp| exp as u64)
}

fn poll_deadline(last_poll: Instant) -> Instant {
    let due = last_poll + Duration::from_secs(1);
    let now = Instant::now();
    if due <= now {
        now + Duration::from_millis(50)
    } else {
        due
    }
}

fn run_daemon_inner(
    config: &NativeConfig,
    plugins: &LoadedPlugins,
    accept: impl Fn(&TcpListener) -> std::io::Result<(std::net::TcpStream, std::net::SocketAddr)>,
    wait: impl Fn(&mut [libc::pollfd], Option<Instant>) -> Result<bool, String>,
) -> Result<(), String> {
    if crate::plugins::handover_stale_daemon(config)? {
        return Ok(());
    }
    let lock = crate::setup::acquire_daemon_owner(&config.state_dir)?;
    fs::set_permissions(&config.state_dir, fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    let listener =
        TcpListener::bind(("127.0.0.1", config.daemon_port)).map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let mut pipe = [-1; 2];
    // SAFETY: pipe points at two writable descriptors; flags keep signal writes nonblocking.
    if unsafe { libc::pipe(pipe.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let read_pipe = unsafe { std::fs::File::from_raw_fd(pipe[0]) };
    let write_pipe = unsafe { std::fs::File::from_raw_fd(pipe[1]) };
    unsafe {
        libc::fcntl(write_pipe.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
        libc::fcntl(read_pipe.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
        libc::fcntl(write_pipe.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(read_pipe.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
    }
    let service = DaemonService::open(config, plugins)?;
    service.state.db.execute(
        "INSERT OR REPLACE INTO daemon_owner VALUES(1,?,?)",
        &[json!(std::process::id()), json!(config.daemon_port)],
    )?;
    let service = Arc::new(Mutex::new(service));
    SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
    SIGNAL_PIPE.store(write_pipe.as_raw_fd(), Ordering::SeqCst);
    let previous_term = unsafe {
        libc::signal(
            libc::SIGTERM,
            handle_daemon_sig as *const () as libc::sighandler_t,
        )
    };
    let previous_int = unsafe {
        libc::signal(
            libc::SIGINT,
            handle_daemon_sig as *const () as libc::sighandler_t,
        )
    };
    let mut ui = spawn_ui(service.clone());
    let mut retired_ui: Vec<UiChild> = Vec::new();
    let health = Arc::new(
        json!({"ok":true,"pid":std::process::id(),"stateDir":config.state_dir,"version":CITO_VERSION}),
    );
    let mut calls: Vec<thread::JoinHandle<()>> = Vec::new();
    let mut last_poll = Instant::now();
    let mut poll_requested = true;
    let mut poll_worker: Option<thread::JoinHandle<()>> = None;
    let mut ui_backoff = crate::support::RetryBackoff::default();
    let mut ui_retry = (ui.is_none() && std::env::var("CITO_DISABLE_UI").as_deref() != Ok("1"))
        .then(|| Instant::now() + ui_backoff.failed());
    let mut ui_started = Instant::now();
    let serving = 'running: loop {
        if shutdown_requested() {
            break Ok(());
        }
        loop {
            match accept(&listener) {
                Ok((stream, _)) => {
                    let s = service.clone();
                    if calls.iter().filter(|call| !call.is_finished()).count() >= 64 {
                        let mut stream = stream;
                        respond(
                            &mut stream,
                            503,
                            &json!({"error":"Too many daemon connections"}),
                        );
                    } else {
                        let health = health.clone();
                        calls.push(thread::spawn(move || handle_http(stream, s, health)));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => break 'running Err(format!("accept: {e}")),
            }
        }
        if poll_worker.as_ref().is_some_and(|w| w.is_finished()) {
            if let Some(worker) = poll_worker.take() {
                let _ = worker.join();
            }
        }
        if (poll_requested || last_poll.elapsed() >= Duration::from_secs(1))
            && poll_worker.is_none()
        {
            let service = Arc::clone(&service);
            poll_worker = Some(thread::spawn(move || {
                if let Ok(mut s) = service.lock() {
                    if let Err(e) = s.poll() {
                        eprintln!("cito poll: {e}");
                    }
                }
            }));
            last_poll = Instant::now();
            poll_requested = false;
        }
        let mut pending = Vec::new();
        for call in calls.drain(..) {
            if call.is_finished() {
                let _ = call.join();
            } else {
                pending.push(call);
            }
        }
        calls = pending;
        if ui.as_mut().is_some_and(|u| {
            if u.mailbox.closed.load(Ordering::SeqCst) {
                let _ = u.child.kill();
            }
            u.child.try_wait().ok().flatten().is_some()
        }) {
            let restart = if let Ok(mut s) = service.try_lock() {
                s.disconnect_ui();
                !s.closing && !shutdown_requested()
            } else {
                false
            };
            if restart {
                if let Some(old) = ui.take() {
                    retired_ui.push(old);
                }
                if ui_started.elapsed() >= Duration::from_secs(30) {
                    ui_backoff.reset();
                }
                ui_retry = Some(Instant::now() + ui_backoff.failed());
            }
        }
        if ui.is_none() && ui_retry.is_some_and(|at| at <= Instant::now()) {
            ui = spawn_ui(service.clone());
            ui_started = Instant::now();
            ui_retry = if ui.is_none() {
                Some(Instant::now() + ui_backoff.failed())
            } else {
                None
            };
        }
        let mut pending_ui = Vec::new();
        for old in retired_ui.drain(..) {
            if old.reader.is_finished() && old.writer.is_finished() {
                let _ = old.reader.join();
                let _ = old.writer.join();
            } else {
                pending_ui.push(old);
            }
        }
        retired_ui = pending_ui;
        let mut fds = [
            libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: read_pipe.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let next = poll_deadline(if poll_worker.is_some() {
            Instant::now()
        } else {
            last_poll
        });
        let deadline = ui_retry.unwrap_or(next).min(next);
        if let Err(error) = wait(&mut fds, Some(deadline)) {
            break Err(format!("poll: {error}"));
        }
        if fds[1].revents != 0 {
            let mut bytes = [0u8; 256];
            // SAFETY: the read end is live and nonblocking; coalesced hints are drained together.
            while unsafe {
                libc::read(
                    read_pipe.as_raw_fd(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            } > 0
            {}
            poll_requested = true;
        }
    };
    request_shutdown();
    drop(listener);
    if let Some(worker) = poll_worker {
        let _ = worker.join();
    }
    // Set closing before draining calls, then close the final host server after drain.
    if let Ok(mut s) = service.lock() {
        s.closing = true;
        s.cancel_logins();
    }
    for call in calls {
        let _ = call.join();
    }
    let (uninstall, mut session_activity) = if let Ok(mut s) = service.lock() {
        s.shutdown();
        (s.uninstall, s.session_activity.take())
    } else {
        (None, None)
    };
    // Plugin workers drain after receivers stop, without retaining the service mutex.
    if let Some(activity) = session_activity.as_mut() {
        if let Err(error) = activity.shutdown() {
            eprintln!("cito badge cleanup: {error}");
        }
    }
    for mut ui in ui.take().into_iter().chain(retired_ui) {
        if ui.child.try_wait().ok().flatten().is_none() {
            let _ = ui.child.kill();
        }
        let _ = ui.child.wait();
        let _ = ui.reader.join();
        let _ = ui.writer.join();
    }
    drop(service);
    let cleanup = uninstall.map_or(Ok(()), |purge| {
        super::setup::uninstall_setup_with_owner(
            &config.state_dir,
            &config.config_path,
            &config.home_dir,
            plugins,
            purge,
            Some(&lock),
        )
        .map(|_| ())
    });
    unsafe {
        libc::signal(libc::SIGTERM, previous_term);
        libc::signal(libc::SIGINT, previous_int);
    }
    SIGNAL_PIPE.store(-1, Ordering::SeqCst);
    drop(write_pipe);
    drop(read_pipe);
    serving.and(cleanup)
}

pub use super::cli::run_cli_command;

impl DaemonService {
    pub fn open(config: &NativeConfig, plugins: &LoadedPlugins) -> Result<Self, String> {
        ensure_cli_shims_native(&config.state_dir, plugins)?;
        let discovered = crate::plugins::load_plugins(&config.state_dir);
        let mut active_plugins = plugins.clone();
        for plugin in &mut active_plugins.plugins {
            if plugin.plugin_dir.join("plugin.json").is_file() {
                if let Some(current) = discovered.find_by_id(&plugin.id) {
                    *plugin = current.clone();
                }
            }
        }
        let plugins = &active_plugins;
        let state = LocalState::open(&config.state_dir)?;
        state.db.set_commit_notifier(wake_daemon)?;
        // Daemon construction runs only after acquiring the exclusive owner lock.
        state.recover_dispatch_leases()?;
        let local = Arc::new(HubEngine::open(&config.state_dir)?);
        let host_path = config.state_dir.join("host-id");
        let host_id = match fs::read_to_string(&host_path) {
            Ok(s) => s.trim().to_string(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let s = random_uuid_v4();
                fs::write(&host_path, &s).map_err(|e| e.to_string())?;
                s
            }
            Err(e) => return Err(e.to_string()),
        };
        if !crate::is_valid_transcript_uuid(&host_id) {
            return Err("Invalid host-id UUID".into());
        }
        fs::set_permissions(&host_path, fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        let mut service = Self {
            access: HubAccess {
                config: config.clone(),
                plugins: plugins.clone(),
                state,
                slots: HashMap::new(),
                host_id,
            },
            local,
            hub_controls: HashMap::new(),
            events: Vec::new(),
            receivers: HashMap::new(),
            caller_cache: Mutex::new(HashMap::new()),
            probes: HashMap::new(),
            observations: HashMap::new(),
            session_activity: Some(super::session_activity::SessionActivity::new(
                config, plugins,
            )),
            ui: None,
            ui_generation: 0,
            last_ui: Instant::now() - Duration::from_secs(1),
            closing: false,
            purging: false,
            host_server: None,
            applied_host_config: None,
            uninstall: None,
            active_settings_hub: None,
            menu_open: false,
            directory_open: false,
            directory_request: 0,
            last_ui_revision: Mutex::new(None),
            last_ui_frame: Mutex::new(None),
            ack_backoff: Arc::default(),
            setup_serial: Arc::default(),
            retired_auth: Vec::new(),
            // The first poll or on-demand refresh starts inspections after publication.
            setup_inspections: plugins
                .agent_plugins()
                .into_iter()
                .map(|p| (p.id.clone(), SetupInspection::default()))
                .collect(),
            setup_completion: Arc::new((Mutex::new(0), std::sync::Condvar::new())),
            host_hub_error: None,
            last_hub_errors: HashMap::new(),
            channel_inventories: HashMap::new(),
            activity_filter: "all".into(),
            activity_query: String::new(),
        };
        service.reload_slots()?;
        Ok(service)
    }

    fn inspect_setup(
        &self,
        plugin: &crate::plugins::PluginDescriptor,
    ) -> Result<(bool, bool, Vec<String>), String> {
        self.setup_inspections
            .get(&plugin.id)
            .map(|entry| entry.value.clone())
            .unwrap_or_else(|| Err("Setup inspection pending".into()))
    }

    fn invalidate_setup(&mut self, id: &str) {
        if self.plugins.find_by_id(id).is_none() {
            return;
        }
        self.setup_inspections
            .entry(id.to_owned())
            .or_default()
            .invalidate();
        self.refresh_setup_inspections();
    }

    fn refresh_setup_inspections(&mut self) {
        let mut changed = false;
        for (id, entry) in &mut self.setup_inspections {
            if entry.worker.as_ref().is_some_and(|(_, worker)| {
                !entry.done.load(Ordering::SeqCst) && !worker.is_finished()
            }) {
                continue;
            }
            if let Some((generation, worker)) = entry.worker.take() {
                let value = worker
                    .join()
                    .unwrap_or_else(|_| Err("Setup inspection worker panicked".into()));
                if generation == entry.generation {
                    entry.value = value;
                    changed = true;
                    continue;
                }
            } else if entry.done.load(Ordering::SeqCst) {
                continue;
            }
            // First inspection, explicit invalidation, or superseded worker.
            if let Some(plugin) = self.access.plugins.find_by_id(id).cloned() {
                let generation = entry.generation;
                entry.done.store(false, Ordering::SeqCst);
                let notice = CompletionNotice {
                    done: entry.done.clone(),
                    completion: self.setup_completion.clone(),
                };
                entry.worker = Some((
                    generation,
                    thread::spawn(move || {
                        let _notice = notice;
                        super::setup::try_inspect_plugin_setup(&plugin)
                    }),
                ));
            }
        }
        if changed && self.ui.is_some() {
            self.emit(json!({"op":"settings","settings":self.settings()}));
        }
    }
}
impl HubAccess {
    fn raw_config(&self) -> Result<Value, String> {
        serde_json::from_slice(self.config_snapshot()?.bytes()?)
            .map_err(|e| format!("Invalid config.json: {e}"))
    }

    fn config_snapshot(&self) -> Result<ConfigSnapshot, String> {
        ConfigSnapshot::read(&self.config.config_path)
    }
}
impl DaemonService {
    pub fn get_effective_user(&self) -> Result<String, String> {
        let raw = self.raw_config()?;
        let user = std::env::var("CITO_USER")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| raw["user"].as_str().map(str::to_string))
            .unwrap_or_else(|| self.config.user.clone());
        if !crate::plugins::validate_user(&user) {
            return Err("INVALID_INPUT: Invalid user".into());
        }
        Ok(user.to_ascii_lowercase())
    }

    fn reject_slot_authority(&mut self, name: &str, error: String) -> Result<(), String> {
        self.last_hub_errors.insert(name.to_owned(), (error, false));
        if let Some(old) = self.slots.remove(name) {
            old.close_and_drain()?;
        }
        self.channel_inventories.remove(name);
        for row in self.state.db.query(
            "SELECT * FROM local_listeners WHERE hub=? AND status='ACTIVE'",
            &[json!(name)],
        )? {
            self.state
                .fence_listener_for(&row, "fenced_before_dispatch")?;
        }
        Ok(())
    }

    pub fn reload_slots(&mut self) -> Result<(), String> {
        if self.purging {
            return Err("STATE_ERROR: Local data reset in progress".into());
        }
        if self.closing {
            return Ok(());
        }
        self.retired_auth
            .retain(|recovery| !recovery.worker.is_finished());
        let mut raw = self.raw_config()?;
        self.config.user = self.get_effective_user()?;
        repair_default_hub(&mut raw);
        let (default, hubs, rest) =
            crate::plugins::try_normalize_existing_config(Some(&raw.to_string()))?;
        let host = rest.get("host_hub").unwrap_or(&Value::Null);
        let _ = self.sync_host_hub(host);
        let address = self.host_server.as_ref().map(|server| server.address);
        self.config.default_hub = std::env::var("CITO_HUB")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or(default);
        // Only a previously configured profile proves removal. Unconfigured
        // legacy recovery rows are retained; disabled profiles remain configured.
        let removed: Vec<_> = self
            .hub_controls
            .keys()
            .filter(|name| !hubs.contains_key(*name))
            .cloned()
            .collect();
        for name in removed {
            self.purge_hub_state(&name)?;
        }
        // Release disabled authorities before checking new admissions. A profile
        // can hand an endpoint to another profile in one external config edit.
        let inactive: Vec<_> = self
            .slots
            .keys()
            .filter(|k| hubs.get(*k).is_none_or(|h| h["enabled"] == false))
            .cloned()
            .collect();
        for name in inactive {
            self.channel_inventories.remove(&name);
            if let Some(slot) = self.slots.remove(&name) {
                slot.close_and_drain()?;
            }
        }
        let mut keep = HashSet::new();
        for (name, mut hub) in hubs {
            crate::plugins::apply_hub_url_override(&name, &self.config.default_hub, &mut hub)?;
            let control = self.hub_controls.entry(name.clone()).or_default();
            if hub["enabled"] == false {
                continue;
            }
            if name != "local" && host_hub::is_own_host_hub_url(string(&hub, "url"), address) {
                self.reject_slot_authority(&name, host_hub::OWN_HUB_ERROR.into())?;
                continue;
            }
            let access = &self.access;
            if control.login.is_some()
                && access.slots.get(&name).is_some_and(|old| old.config == hub)
            {
                keep.insert(name);
                continue;
            }
            let auth = string(&hub, "authMode");
            let user = access.config.effective_user(&name)?.to_ascii_lowercase();
            keep.insert(name.clone());
            let old_slot = access.slots.get(&name);
            let rejected_hash = old_slot.filter(|old| old.config == hub).and_then(|old| {
                old.rejected_bearer
                    .lock()
                    .ok()
                    .and_then(|hash| hash.clone())
            });
            let mut recovered = None;
            if let Some(pending) = control.recovery.take() {
                if pending.config != hub || pending.rejected != rejected_hash {
                    pending.cancelled.store(true, Ordering::SeqCst);
                    self.retired_auth.push(pending);
                } else if pending.worker.is_finished() {
                    recovered = Some(pending.worker.join().unwrap_or_else(|_| {
                        Err("AUTH_UNAVAILABLE: recovery worker panicked".into())
                    }));
                } else {
                    control.recovery = Some(pending);
                    if old_slot.is_some_and(|old| {
                        old.config == hub
                            && rejected_hash.is_none()
                            && old.expires_at > now_ms() / 1000
                    }) {
                        continue;
                    }
                }
            }
            if recovered.is_none()
                && old_slot.is_some_and(|old| {
                    old.config == hub
                        && (auth == "oidc" || string(&old.identity, "owner_user") == user)
                        && (auth != "oidc"
                            || (old.auth_retry_at > now_ms()
                                && (old.expires_at == 0
                                    || (rejected_hash.is_none()
                                        && old.expires_at > now_ms() / 1000)))
                            || (rejected_hash.is_none() && old.expires_at > now_ms() / 1000 + 30))
                })
            {
                continue;
            }
            let mut previous = old_slot.cloned();
            let mut retired_rows = Vec::new();
            if let Some(old) = &previous {
                let changed = old.config["url"] != hub["url"]
                    || (auth != "oidc"
                        && !super::auth::principal_ids_equal(
                            string(&old.identity, "principal_id"),
                            &format!("none#{user}"),
                        ));
                if changed {
                    for row in access
                        .state
                        .active()?
                        .into_iter()
                        .filter(|r| r["hub"] == name)
                    {
                        if old.local.is_some() {
                            let channel = format!("{}:{}", access.host_id, string(&row, "harness"));
                            let _ = detach_channel_expected(
                                old,
                                string(&row, "listener_id"),
                                &channel,
                                &row,
                            );
                        } else {
                            retired_rows.push(row.clone());
                        }
                        access
                            .state
                            .fence_listener_for(&row, "fenced_before_dispatch")?;
                    }
                }
            }
            let mut client = HubClient::new(
                name.clone(),
                hub.clone(),
                &user,
                if hub["url"] == "local://" {
                    Some(self.local.clone())
                } else {
                    None
                },
            );
            if auth == "oidc" {
                if recovered.is_none() && control.recovery.is_none() {
                    let dir = self.access.config.state_dir.clone();
                    let hub_name = name.clone();
                    let config = hub.clone();
                    let rejected = rejected_hash.clone();
                    let cancelled = Arc::new(super::login::Cancellation::default());
                    let cancel = cancelled.clone();
                    let worker = thread::spawn(move || {
                        super::login::recover_cancellable(
                            &dir,
                            &hub_name,
                            &config,
                            rejected.as_deref(),
                            false,
                            false,
                            (
                                &|| cancel.load(Ordering::SeqCst) || shutdown_requested(),
                                Some(cancel.fd()?),
                            ),
                        )
                    });
                    control.recovery = Some(OidcRecovery {
                        config: hub.clone(),
                        rejected: rejected_hash.clone(),
                        cancelled,
                        worker,
                    });
                }
                let failed = recovered.as_ref().is_some_and(|result| result.is_err());
                let retry_delay = if failed {
                    control.backoff.failed().as_millis() as u64
                } else {
                    1000
                };
                // A background refresh cannot revoke an unexpired, accepted credential.
                if recovered.as_ref().is_none_or(|result| result.is_err())
                    && rejected_hash.is_none()
                    && previous.as_ref().is_some_and(|old| {
                        old.config == hub
                            && old.expires_at > now_ms() / 1000
                            && old.bearer.is_some()
                    })
                {
                    if let Some(mut old) = previous.take() {
                        old.auth_retry_at = now_ms() + retry_delay;
                        self.slots.insert(name.clone(), old);
                    }
                    continue;
                }
                match recovered.unwrap_or_else(|| Err("AUTH_UNAVAILABLE: recovery pending".into()))
                {
                    Ok((token, identity)) => {
                        control.backoff.reset();
                        client.expires_at = jwt_exp(&token).unwrap_or(0);
                        client.bearer = Some(token);
                        client.identity = identity;
                    }
                    Err(_) => {
                        client.bearer = None;
                        client.expires_at = 0;
                        client.auth_retry_at = now_ms() + retry_delay;
                        *client
                            .rejected_bearer
                            .lock()
                            .map_err(|_| "Rejected bearer lock poisoned")? = rejected_hash.clone();
                        if let Some(identity) =
                            super::login::offline_identity(&self.config.state_dir, &name, &hub)
                        {
                            client.identity = identity;
                        }
                        if let Ok(mut status) = client.status.lock() {
                            *status = "auth_required".into();
                        }
                    }
                }
            }
            // Retained slots already passed admission; check newly resolved identities
            // before publishing a replacement or starting its event stream.
            // An OIDC client starts with the static user's placeholder identity.
            // Both sides of a collision must have an established principal.
            let established = |slot: &HubClient| {
                slot.config["authMode"] != "oidc" || slot.identity["oidc"] == true
            };
            if let Some(endpoint) = host_hub::remote_endpoint(&hub).filter(|_| established(&client))
            {
                if let Some(existing) = self.slots.values().find(|slot| {
                    slot.name != name
                        && established(slot)
                        && host_hub::remote_endpoint(&slot.config).as_ref() == Some(&endpoint)
                        && super::auth::principal_ids_equal(
                            string(&slot.identity, "principal_id"),
                            string(&client.identity, "principal_id"),
                        )
                }) {
                    self.reject_slot_authority(
                        &name,
                        host_hub::duplicate_hub_error(&existing.name),
                    )?;
                    continue;
                }
            }
            // A formerly suppressed slot has recovered. Preserve unrelated
            // diagnostics from rejected edits on disabled or disconnected hubs.
            if previous.is_none()
                && self.last_hub_errors.get(&name).is_some_and(|(err, _)| {
                    err == host_hub::OWN_HUB_ERROR || err.starts_with(host_hub::DUPLICATE_HUB_ERROR)
                })
            {
                self.last_hub_errors.remove(&name);
            }
            let retained = previous.as_ref().is_some_and(|old| {
                old.config["url"] == client.config["url"]
                    && rejected_hash.is_none()
                    && old.expires_at > now_ms() / 1000
                    && client.expires_at > now_ms() / 1000
                    && super::auth::principal_ids_equal(
                        string(&old.identity, "principal_id"),
                        string(&client.identity, "principal_id"),
                    )
            });
            if let Some(old) = &previous {
                if retained {
                    client.inherit_authority(old);
                } else {
                    old.close_and_drain()?;
                    // A failed fence must leave the replacement eligible for retry.
                    self.slots.remove(&name);
                }
            }
            // Cleanup uses the old profile and channel token. Tie cancellation to
            // the replacement's lifetime so a further switch or shutdown reaps it.
            if let Some(mut cleanup) = previous.clone().filter(|_| !retired_rows.is_empty()) {
                cleanup.closed = client.closed.clone();
                let host = self.host_id.clone();
                self.events.push(thread::spawn(move || {
                    for row in retired_rows {
                        if cleanup.closed.load(Ordering::Acquire) {
                            break;
                        }
                        let channel = format!("{host}:{}", string(&row, "harness"));
                        let _ = detach_channel_expected(
                            &cleanup,
                            string(&row, "listener_id"),
                            &channel,
                            &row,
                        );
                    }
                }));
            }
            // Old writers check the close fence inside their SQLite transaction.
            // This query observes any commit admitted before close.
            // Disabled profiles have no previous slot. Durable binding identity must
            // still be checked before any receiver can use the replacement client.
            for row in self.state.db.query(
                "SELECT * FROM local_listeners WHERE hub=? AND status != \'FENCED_INACTIVE\'",
                &[json!(name)],
            )? {
                let changed = !super::auth::principal_ids_equal(
                    string(&row, "principal_id"),
                    string(&client.identity, "principal_id"),
                ) || row["hub_url"]
                    .as_str()
                    .is_some_and(|url| url != string(&hub, "url"));
                if changed {
                    self.state
                        .fence_listener_for(&row, "fenced_before_dispatch")?;
                } else if row["hub_url"].is_null() {
                    // Legacy bindings predate URL persistence; record the first known profile.
                    self.state.db.execute("UPDATE local_listeners SET hub_url=? WHERE hub=? AND listener_id=? AND hub_url IS NULL", &[hub["url"].clone(),row["hub"].clone(),row["listener_id"].clone()])?;
                }
            }
            if !retained {
                self.channel_inventories.remove(&name);
            }
            if !retained && (auth != "oidc" || client.bearer.is_some()) {
                if let Some(events) = client.start_events() {
                    self.events.push(events);
                }
            }
            self.slots.insert(name.clone(), client);
        }
        for (name, control) in &mut self.hub_controls {
            if keep.contains(name) {
                continue;
            }
            if let Some(pending) = control.recovery.take() {
                pending.cancelled.store(true, Ordering::SeqCst);
                self.retired_auth.push(pending);
            }
        }
        if self
            .active_settings_hub
            .as_ref()
            .is_some_and(|hub| !self.slots.contains_key(hub))
        {
            self.active_settings_hub = None;
        }
        self.ensure_human_nodes()?;
        Ok(())
    }

    pub(super) fn reconcile_unlistened(
        state: &LocalState,
        client: &HubClient,
        host_id: &str,
    ) -> Result<(), String> {
        // Re-read durable intent under the enrollment lock: an explicit re-enable
        // supersedes any pending detach before this worker can mutate the Hub.
        let registration = state.registration_lock(&client.name)?;
        let _registration = registration
            .lock()
            .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
        Self::reconcile_unlistened_locked(state, client, host_id)
    }

    fn reconcile_unlistened_locked(
        state: &LocalState,
        client: &HubClient,
        host_id: &str,
    ) -> Result<(), String> {
        for pending in state.db.query(
            "SELECT * FROM pending_hub_detaches WHERE hub=?",
            &[json!(client.name)],
        )? {
            if client.closed.load(Ordering::Acquire) {
                return Err("DETACH_PENDING: Hub changed before detach".into());
            }
            let id = string(&pending, "listener_id");
            let Some(row) = state
                .listener(&client.name, id)?
                .filter(|row| row["status"] == "UNLISTENED")
            else {
                state.db.execute("DELETE FROM pending_hub_detaches WHERE hub=?1 AND listener_id=?2 AND NOT EXISTS (SELECT 1 FROM local_listeners WHERE hub=?1 AND listener_id=?2 AND status='UNLISTENED')", &[json!(client.name),json!(id)])?;
                continue;
            };
            // Fence the local intent, including a pre-enrollment Human pause whose
            // remote detach must adopt the remote generation and owner.
            let clear = || {
                state.clear_pending_detach_if_current(
                    &client.name,
                    id,
                    row["binding_generation"].as_u64().unwrap_or(0),
                    string(&row, "owner_token"),
                )
            };
            if pending["hub_url"].as_str() != Some(string(&client.config, "url")) {
                clear()?;
                continue;
            }
            if !super::auth::principal_ids_equal(
                string(&row, "principal_id"),
                string(&client.identity, "principal_id"),
            ) {
                continue;
            }
            let channel = format!("{host_id}:{}", string(&row, "harness"));
            let Some((remote, _)) = client.kv_get(id)? else {
                clear()?;
                continue;
            };
            let human = row["harness"] == "human";
            let captured = if human
                && row["binding_generation"] == 0
                && id == listener_id(string(&client.identity, "principal_id"), "human", "default")
            {
                // Pause can precede automatic enrollment on this Mac.
                let mut captured = remote.clone();
                if let Some(owner) = remote["channels"][&channel]["owner_token"].as_str() {
                    captured["owner_token"] = json!(owner);
                    captured["binding_generation"] =
                        remote["channels"][&channel]["binding_generation"].clone();
                }
                captured
            } else {
                row.clone()
            };
            if !super::registration::remote_matches_listener(&remote, &captured, &channel) {
                clear()?;
                continue;
            }
            if remote["channels"][&channel]["state"] == "ACTIVE" {
                detach_channel_expected(client, id, &channel, &captured)?;
            }
            clear()?;
        }
        Ok(())
    }

    // Callers hold the Hub registration lock across the authoritative read/write
    // and projection, so an older snapshot cannot overwrite a newer policy result.
    pub(super) fn reconcile_human_scope(
        state: &LocalState,
        client: &HubClient,
        host_id: &str,
        remote: &Value,
    ) -> Result<(), String> {
        if remote["harness"] != "human" || client.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let Some(row) = state.listener(&client.name, string(remote, "listener_id"))? else {
            return Ok(());
        };
        let channel = format!("{host_id}:human");
        if row["status"] == "ACTIVE"
            && super::auth::principal_ids_equal(
                string(&row, "principal_id"),
                string(&client.identity, "principal_id"),
            )
            && super::registration::remote_matches_listener(remote, &row, &channel)
        {
            state.db.transaction(|db| {
                client.check_current()?;
                db.execute("UPDATE local_listeners SET scope=? WHERE hub=? AND listener_id=? AND status='ACTIVE' AND binding_generation=? AND owner_token=?", &[remote["scope"].clone(),row["hub"].clone(),row["listener_id"].clone(),row["binding_generation"].clone(),row["owner_token"].clone()])?;
                Ok(())
            })?;
        }
        Ok(())
    }

    fn ensure_human_nodes(&mut self) -> Result<(), String> {
        for (slot_name, client) in self.slots.clone() {
            let registration = self.state.registration_lock(&slot_name)?;
            let _local_guard = if client.local.is_some() {
                let Some(guard) = try_registration(&registration)? else {
                    continue;
                };
                let _ = Self::reconcile_unlistened_locked(&self.state, &client, &self.host_id);
                Some(guard)
            } else {
                None
            };
            if client.config["authMode"] == "oidc" && client.bearer.is_none() {
                continue;
            }
            if super::registration::local_node(&self.state, &client, "human", "default")?
                .is_some_and(|r| r["status"] == "ACTIVE")
            {
                continue;
            }
            let args = json!({"about":"Human receiver"});
            let caller = json!({"harness":"human","session_id":"default","cwd":"","title":"Human"});
            if client.local.is_some() {
                match super::registration::register_automatic_human_locked(
                    &self.state,
                    &client,
                    &self.host_id,
                    &args,
                    &caller,
                ) {
                    // A Hub-wide pause can predate this local profile. Enrollment is
                    // optional; preserving that pause must not reject unrelated settings.
                    Err(error) if error.starts_with("BINDING_REQUIRED:") => {}
                    result => {
                        result?;
                    }
                }
                continue;
            }
            let key = format!("auto:{slot_name}");
            if self.receivers.contains_key(&key) {
                continue;
            }
            let state = self.state.clone();
            let host = self.host_id.clone();
            self.receivers.insert(
                key,
                thread::spawn(move || {
                    let _ = super::registration::register_automatic_human(
                        &state, &client, &host, &args, &caller,
                    );
                }),
            );
        }
        Ok(())
    }
}
impl HubAccess {
    fn slot(&self, name: &str) -> Result<&HubClient, String> {
        self.slots
            .get(name)
            .ok_or_else(|| format!("INVALID_INPUT: Unknown or disabled hub: {name}"))
    }

    fn scoped(&self, hub: &str, id: &str) -> String {
        if self.slots.len() == 1
            && hub == self.config.default_hub
            && matches!(hub, "local" | "default")
        {
            id.into()
        } else {
            format!("{hub}:{id}")
        }
    }
}
impl DaemonService {
    pub fn poll(&mut self) -> Result<(), String> {
        if self.purging {
            return Ok(());
        }
        self.events.retain(|event| !event.is_finished());
        self.refresh_setup_inspections();
        self.reload_slots()?;
        self.flush_acks();
        // Successful background RPCs or event reconnection supersede transport
        // errors, but cannot resolve a rejected configuration edit.
        self.last_hub_errors.retain(|hub, (_, transient)| {
            !*transient
                || self
                    .access
                    .slots
                    .get(hub)
                    .is_none_or(|slot| slot.connection_status() != "connected")
        });
        let finished: Vec<String> = self
            .receivers
            .iter()
            .filter(|(_, t)| t.is_finished())
            .map(|(k, _)| k.clone())
            .collect();
        for key in finished {
            if let Some(thread) = self.receivers.remove(&key) {
                let _ = thread.join();
            }
        }
        // The registry owns background tasks; the per-hub owner serializes these
        // tasks with explicit reads and mutations outside the service mutex.
        if let Some(slot) = self
            .active_settings_hub
            .as_ref()
            .and_then(|hub| self.slots.get(hub))
            .filter(|slot| slot.local.is_none())
            .cloned()
        {
            let key = format!("channels:{}", slot.name);
            let inventory = self.channel_inventory(&slot);
            let (due, observed_gen) = {
                let cached = inventory.cached.lock().map_err(|e| e.to_string())?;
                (
                    cached.authorized_for_retry(&slot)
                        && (cached.value["status"] == "loading"
                            || cached.retry_at <= Instant::now()),
                    cached.completed_gen,
                )
            };
            if due && !self.receivers.contains_key(&key) {
                self.receivers.insert(
                    key,
                    thread::spawn(move || {
                        let _ = inventory.refresh(&slot, observed_gen);
                        wake_daemon();
                    }),
                );
            }
        }
        for (hub, slot) in &self.access.slots {
            let key = format!("directory:{hub}");
            let revision = slot.event_revision();
            let due = self
                .observations
                .get(&key)
                .is_none_or(|(seen, at)| *seen != revision || *at <= Instant::now());
            if due
                && !self.receivers.contains_key(&key)
                && (slot.config["authMode"] != "oidc" || slot.bearer.is_some())
            {
                self.observations.insert(
                    key.clone(),
                    (revision, Instant::now() + Duration::from_secs(30)),
                );
                let slot = slot.clone();
                let state = self.state.clone();
                let host_id = self.host_id.clone();
                self.receivers.insert(
                    key,
                    thread::spawn(move || {
                        let refreshed = (|| -> Result<(), String> {
                            slot.directory()?;
                            let registration = state.registration_lock(&slot.name)?;
                            let _registration = registration
                                .lock()
                                .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
                            if slot.closed.load(Ordering::Acquire) {
                                return Ok(());
                            }
                            // A Human policy write may have advanced the cache while we
                            // waited. Project its current record, not the earlier RPC result.
                            for remote in slot.cached_directory() {
                                Self::reconcile_human_scope(&state, &slot, &host_id, &remote)?;
                            }
                            Ok(())
                        })();
                        wake_daemon();
                        if refreshed.is_ok() && slot.local.is_none() {
                            let _ = Self::reconcile_unlistened(&state, &slot, &host_id);
                        }
                    }),
                );
            }
        }
        self.schedule_probes()?;
        let active = self.state.active()?;
        let keys: HashSet<String> = active
            .iter()
            .map(|row| format!("{}:{}", string(row, "hub"), string(row, "listener_id")))
            .chain(self.slots.keys().map(|hub| format!("directory:{hub}")))
            .collect();
        self.observations.retain(|key, _| keys.contains(key));
        for row in active {
            let hub = string(&row, "hub");
            let key = format!("{hub}:{}", string(&row, "listener_id"));
            if self.receivers.contains_key(&key) {
                continue;
            }
            let Ok(slot) = self.slot(hub).cloned() else {
                continue;
            };
            if slot.config["authMode"] == "oidc" && slot.bearer.is_none() {
                continue;
            }
            let revision = slot.event_revision();
            let retry_due = self
                .state
                .db
                .query(
                    "SELECT retry_at_ms FROM wake_retries WHERE hub=? AND listener_id=?",
                    &[row["hub"].clone(), row["listener_id"].clone()],
                )?
                .first()
                .and_then(|r| r["retry_at_ms"].as_u64())
                .is_some_and(|at| at <= now_ms());
            let channel = format!("{}:{}", self.host_id, string(&row, "harness"));
            let heartbeat_due = slot
                .cached_directory()
                .iter()
                .find(|remote| remote["listener_id"] == row["listener_id"])
                .and_then(|remote| remote["channels"][&channel]["updated_at_ms"].as_u64())
                .is_some_and(|at| slot.server_now().saturating_sub(at) >= 20_000);
            if !retry_due
                && !heartbeat_due
                && self
                    .observations
                    .get(&key)
                    .is_some_and(|(seen, at)| *seen == revision && *at > Instant::now())
            {
                continue;
            }
            self.observations.insert(
                key.clone(),
                (revision, Instant::now() + Duration::from_secs(30)),
            );
            let state = self.state.clone();
            let state_dir = self.config.state_dir.clone();
            let host_id = self.host_id.clone();
            let plugin = self.plugins.find_by_id(string(&row, "harness")).cloned();
            self.receivers.insert(key,thread::spawn(move||{
                let channel=format!("{}:{}",host_id,string(&row,"harness"));
                if row["harness"] == "human" {
                    let updated = slot.cached_directory().iter()
                        .find(|record| record["listener_id"] == row["listener_id"])
                        .and_then(|record| record["channels"][&channel]["updated_at_ms"].as_u64());
                    if updated.is_none_or(|updated| slot.server_now().saturating_sub(updated) >= 20_000) {
                    if let Err(error) = slot.human_request("human_device_heartbeat",&json!({"node_id":row["listener_id"],"channel_id":channel,"binding_generation":row["binding_generation"],"owner_token":row["owner_token"]})) {
                        if error.starts_with("STALE_GENERATION:") { let _ = slot.with_current(||state.fence_listener_for(&row,"fenced_before_dispatch")); }
                        return;
                    }
                    }
                } else if !Self::heartbeat_agent(&state, &slot, &channel, &row).unwrap_or(false) {
                    return;
                }
                if let Err(error)=poll_listener(&state,&slot,&row,plugin.as_ref(),&state_dir, |_| {}) {
                    if !slot.closed.load(Ordering::SeqCst) { eprintln!("cito receiver {}: {error}",slot.name); }
                }
            }));
        }
        self.update_discovery_demand();
        let live = self.live_receivers()?;
        let changed = self
            .session_activity
            .as_mut()
            .map(|activity| activity.tick(&self.access.state, &live))
            .transpose()?
            .unwrap_or(false);
        self.auto_listen()?;
        if changed || self.last_ui.elapsed() >= Duration::from_secs(1) {
            self.push_ui();
            self.last_ui = Instant::now();
        }
        Ok(())
    }

    fn heartbeat_agent(
        state: &LocalState,
        slot: &HubClient,
        channel: &str,
        queued: &Value,
    ) -> Result<bool, String> {
        let id = string(queued, "listener_id");
        let lock = state.registration_lock(&slot.name)?;
        let _guard = lock
            .lock()
            .map_err(|_| "STATE_ERROR: Registration lock poisoned")?;
        let Some(row) = state.listener(&slot.name, id)?.filter(|row| {
            row["status"] == "ACTIVE"
                && row["binding_generation"] == queued["binding_generation"]
                && row["owner_token"] == queued["owner_token"]
        }) else {
            return Ok(false);
        };
        if let Ok(Some((mut remote, revision))) = slot.kv_get(id) {
            if !super::registration::remote_matches_listener(&remote, &row, channel)
                || remote["channels"][channel]["state"] != "ACTIVE"
            {
                slot.with_current(|| state.fence_listener_for(&row, "fenced_before_dispatch"))?;
                return Ok(false);
            }
            state.db.transaction(|db| {
                slot.check_current()?;
                db.execute("UPDATE local_listeners SET scope=?1 WHERE hub=?2 AND listener_id=?3 AND binding_generation=?4 AND owner_token=?5 AND scope IS NOT ?1", &[remote["scope"].clone(),row["hub"].clone(),row["listener_id"].clone(),row["binding_generation"].clone(),row["owner_token"].clone()])?;
                Ok(())
            })?;
            let mut changed = false;
            if let Ok(Some(title)) =
                state.custom_title(string(&row, "harness"), string(&row, "session_id"))
            {
                // A failed naming-intent save leaves the newly committed binding title intact.
                if row["title"] == title
                    && (remote["title"] != title || remote["custom_title"] != true)
                {
                    remote["title"] = json!(title);
                    remote["custom_title"] = json!(true);
                    changed = true;
                }
            }
            if let Some(ch) = remote["channels"]
                .get_mut(channel)
                .and_then(Value::as_object_mut)
            {
                let now = slot.server_now();
                if now.saturating_sub(ch.get("updated_at_ms").and_then(Value::as_u64).unwrap_or(0))
                    >= 20_000
                {
                    ch.insert("updated_at_ms".into(), json!(now));
                    changed = true;
                }
            }
            if changed {
                let _ = slot.kv_save(id, &remote, Some(revision));
            }
        }
        Ok(true)
    }

    fn schedule_probes(&mut self) -> Result<(), String> {
        let mut groups = std::collections::BTreeMap::<String, Vec<Value>>::new();
        for row in self
            .state
            .active()?
            .into_iter()
            .filter(|row| row["harness"] != "human")
        {
            let start = row["endpoint_pid"]
                .as_u64()
                .and_then(|pid| crate::support::process_start(pid as u32));
            let key = json!([
                row["harness"],
                row["session_id"],
                row["endpoint_pid"],
                row["process_identity"],
                row["cwd"],
                start
            ])
            .to_string();
            groups.entry(key).or_default().push(row);
        }
        self.probes.retain(|key, probe| {
            groups.contains_key(key)
                || probe
                    .worker
                    .as_ref()
                    .is_some_and(|worker| !worker.is_finished())
        });
        for (key, rows) in groups {
            let bindings: Vec<String> = rows
                .iter()
                .map(|row| -> Result<String,String> {
                    let pending = !self.state.db.query("SELECT 1 FROM local_signals WHERE hub=? AND listener_id=? AND state IN ('RECEIVED','DISPATCHING') LIMIT 1", &[row["hub"].clone(),row["listener_id"].clone()])?.is_empty();
                    Ok(json!([row["hub"], row["binding_generation"], row["owner_token"],pending]).to_string())
                })
                .collect::<Result<_,_>>()?;
            let probe = self.probes.entry(key).or_insert_with(|| EndpointProbe {
                due: Instant::now(),
                calm: 0,
                bindings: bindings.clone(),
                worker: None,
            });
            if probe
                .worker
                .as_ref()
                .is_some_and(|worker| worker.is_finished())
            {
                let observation = probe
                    .worker
                    .take()
                    .and_then(|worker| worker.join().ok().flatten());
                let quiet = observation.is_some();
                if let (Some(activity), Some(observation)) =
                    (&mut self.session_activity, observation)
                {
                    activity.observe_badge(&observation);
                }
                probe.calm = if quiet {
                    probe.calm.saturating_add(1).min(5)
                } else {
                    0
                };
                probe.due = Instant::now() + Duration::from_secs((1u64 << probe.calm).min(30));
            }
            if bindings != probe.bindings {
                probe.bindings = bindings;
                probe.due = Instant::now();
                probe.calm = 0;
            }
            if probe.worker.is_some() || probe.due > Instant::now() {
                continue;
            }
            let Some(plugin) = self
                .access
                .plugins
                .find_by_id(string(&rows[0], "harness"))
                .cloned()
            else {
                continue;
            };
            let state = self.access.state.clone();
            let dir = self.access.config.state_dir.clone();
            let targets: Vec<_> = rows
                .into_iter()
                .filter_map(|row| {
                    self.access
                        .slots
                        .get(string(&row, "hub"))
                        .cloned()
                        .map(|client| (row, client))
                })
                .collect();
            if targets.is_empty() {
                continue;
            }
            probe.worker = Some(thread::spawn(move || {
                let row = &targets[0].0;
                let result = super::receiver::plugin_request(
                    &plugin,
                    &json!({"op":"probe","session_id":row["session_id"],"endpoint_pid":row["endpoint_pid"],"process_identity":row["process_identity"],"cwd":row["cwd"],"state_dir":dir}),
                    None,
                    None,
                );
                let quiet = result
                    .as_ref()
                    .is_ok_and(|result| result["ok"] == true && result["status"] == "alive");
                let result = result.unwrap_or_else(|error| json!({"ok":false,"error":error}));
                let mut observation = targets[0].0.clone();
                if let Some(badge) = result.get("badge") {
                    observation["badge"] = badge.clone();
                }
                for (row, client) in targets {
                    let _ = super::receiver::apply_probe(&state, &client, &row, &result);
                }
                wake_daemon();
                quiet.then_some(observation)
            }));
        }
        Ok(())
    }

    // The outer result owns the config commit; the inner result owns only its
    // secondary local projection, which cannot turn a confirmed save into failure.
    fn set_auto_listen_locked(
        &mut self,
        hub: &str,
        enabled: bool,
        detaches: &mut mcp::Detaches,
    ) -> Result<Result<(), String>, String> {
        if !self.slots.contains_key(hub) {
            let raw = self.raw_config()?;
            let (_, hubs, _) =
                crate::plugins::try_normalize_existing_config(Some(&raw.to_string()))?;
            if !hubs.contains_key(hub) {
                return Err(format!("INVALID_INPUT: Unknown hub: {hub}"));
            }
        }
        // The captured policy edit owns registration until all local intents commit.
        self.mutate_config(|v| {
            if v["hubs"].is_null() {
                v["hubs"] = json!({"local":{"url":"local://"}});
            }
            v["hubs"][hub]["autoListen"] = json!(enabled);
            Ok(())
        })?;
        if let Some(slot) = self.slots.get_mut(hub) {
            slot.config["autoListen"] = json!(enabled);
        }
        let projection = (|| {
            if !enabled {
                for plugin in self.plugins.agent_plugins() {
                    self.state.invalidate_discovery(&plugin.id)?;
                }
            }
            if enabled {
                self.state.db.transaction(|c| {
                    c.execute(
                        "UPDATE local_listeners SET user_stopped=0 WHERE hub=? AND harness!='human'",
                        &[json!(hub)],
                    )?;
                    Ok(())
                })?;
            }
            if !enabled {
                let mut errors = Vec::new();
                for row in self.state.active()?.into_iter().filter(|r| {
                    r["hub"] == hub && r["harness"] != "human" && r["listen_mode"] == "auto"
                }) {
                    if let Err(error) =
                        detaches.retire(self, hub, string(&row, "listener_id"), "auto_disabled")
                    {
                        errors.push(error);
                    }
                }
                if !errors.is_empty() {
                    return Err(format!("DETACH_PENDING: {}", errors.join("; ")));
                }
            }
            if enabled {
                if let Some(activity) = self.session_activity.as_mut() {
                    for plugin in self.access.plugins.agent_plugins() {
                        activity.mark_stale(&plugin.id);
                    }
                    activity.refresh_discovery();
                }
            }
            Ok(())
        })();
        Ok(projection)
    }

    /// Reconcile source activity independently of discovery scans and Hub heartbeats.
    fn auto_listen(&mut self) -> Result<(), String> {
        use super::state::{activity_revision, qualifying_activity};
        use crate::plugins::AutoListenMode;
        let candidates: Vec<Value> = self
            .session_activity
            .as_ref()
            .map(|a| a.candidates().collect())
            .unwrap_or_default();
        for row in self
            .state
            .active()?
            .into_iter()
            .filter(|r| r["harness"] != "human")
        {
            if let Some((revision, at)) = candidates
                .iter()
                .find(|c| c["harness"] == row["harness"] && c["session_id"] == row["session_id"])
                .and_then(qualifying_activity)
                .filter(|(revision, _)| {
                    activity_revision(&row, "last_activity_revision")
                        .is_none_or(|old| *revision > old)
                })
            {
                self.state.db.execute("UPDATE local_listeners SET last_activity_revision=?,last_activity_at_ms=MAX(COALESCE(last_activity_at_ms,0),?) WHERE hub=? AND listener_id=? AND status='ACTIVE'", &[json!(revision.to_string()),json!(at.min(now_ms())),row["hub"].clone(),row["listener_id"].clone()])?;
            }
        }
        // Retirement and dispatch claims share the registration and SQLite locks.
        for row in self
            .state
            .active()?
            .into_iter()
            .filter(|r| r["harness"] != "human" && r["listen_mode"] == "auto")
        {
            let Some(timeout) = self
                .plugins
                .find_by_id(string(&row, "harness"))
                .and_then(|p| p.auto_listen.idle_timeout_ms)
            else {
                continue;
            };
            let hub = string(&row, "hub");
            let lock = self.state.registration_lock(hub)?;
            let Some(_guard) = try_registration(&lock)? else {
                continue;
            };
            let retired = self.state.db.transaction(|c| {
                let now = now_ms();
                let changed = c.execute("UPDATE local_listeners SET status='UNLISTENED',retirement_reason='idle',retired_activity_revision=last_activity_revision,updated_at=?1 WHERE hub=?2 AND listener_id=?3 AND status='ACTIVE' AND listen_mode='auto' AND ?1-COALESCE(last_activity_at_ms,?1)>?4 AND lease_until_ms<=?1 AND NOT EXISTS(SELECT 1 FROM local_signals s WHERE s.hub=?2 AND s.listener_id=?3 AND (((s.state IN ('RECEIVED','DISPATCHING') AND s.held_reason IS NULL OR s.state='STORED_PASSIVE' AND COALESCE(s.held_reason,'') IN ('','driver_unavailable') AND s.binding_generation=local_listeners.binding_generation AND s.hub_url=local_listeners.hub_url AND COALESCE(s.batch_expires_at_ms,s.expires_at_ms)>?1) AND s.acknowledged_at_ms IS NULL AND s.expires_at_ms>?1) OR s.delivered_at_ms+300000>?1))", &[json!(now),json!(hub),row["listener_id"].clone(),json!(timeout)])?;
                if changed > 0 {
                    c.execute("INSERT INTO pending_hub_detaches(hub,listener_id,hub_url) VALUES(?,?,?) ON CONFLICT(hub,listener_id) DO UPDATE SET hub_url=excluded.hub_url", &[json!(hub),row["listener_id"].clone(),row["hub_url"].clone()])?;
                    super::state::reconcile_wake_retry(c, hub, string(&row, "listener_id"))?;
                }
                Ok(changed > 0)
            })?;
            if retired {
                self.state.invalidate_discovery(string(&row, "harness"))?;
                if let Some(slot) = self.slots.get(hub) {
                    if slot.local.is_some() {
                        Self::reconcile_unlistened_locked(&self.state, slot, &self.host_id)?;
                    } else {
                        // The directory worker drains remote detaches outside the service mutex.
                        self.observations.remove(&format!("directory:{hub}"));
                    }
                }
            }
        }
        let mut stale = HashSet::new();
        let mut remote = HashMap::<String, Vec<(Value, Value)>>::new();
        for candidate in candidates {
            let harness = string(&candidate, "harness");
            let Some(plugin) = self.plugins.find_by_id(harness) else {
                continue;
            };
            if self
                .setup_inspections
                .get(harness)
                .is_none_or(|e| e.worker.is_some())
                || !self
                    .inspect_setup(plugin)
                    .is_ok_and(|(_, configured, _)| configured)
                || candidate["needs_reload"] == true
            {
                continue;
            }
            let endpoint_alive = candidate["endpoint_pid"]
                .as_u64()
                .and_then(|p| u32::try_from(p).ok())
                .is_some_and(crate::support::alive);
            if (plugin.auto_listen.mode == AutoListenMode::Endpoint
                || candidate["endpoint_pid"].is_number())
                && !endpoint_alive
            {
                continue;
            }
            for (hub, slot) in &self.slots {
                if slot.connection_status() != "connected" {
                    continue;
                }
                let prior = super::registration::local_node(
                    &self.state,
                    slot,
                    harness,
                    string(&candidate, "session_id"),
                )?;
                if prior
                    .as_ref()
                    .is_some_and(|r| r["status"] == "ACTIVE" || r["user_stopped"] == 1)
                {
                    continue;
                }
                if prior.as_ref().is_some_and(|r| {
                    r["retirement_reason"] == "idle"
                        && !qualifying_activity(&candidate).is_some_and(|(revision, _)| {
                            activity_revision(r, "retired_activity_revision")
                                .is_none_or(|old| revision > old)
                        })
                }) {
                    continue;
                }
                let restoring = prior.as_ref().is_some_and(|r| {
                    matches!(
                        r["retirement_reason"].as_str(),
                        Some("hard_exit" | "disconnected")
                    )
                });
                let manual =
                    restoring && prior.as_ref().is_some_and(|r| r["listen_mode"] == "manual");
                if !manual && slot.config["autoListen"] != true {
                    continue;
                }
                if restoring
                    && !manual
                    && plugin.auto_listen.idle_timeout_ms.is_some_and(|ttl| {
                        prior
                            .as_ref()
                            .and_then(|r| r["last_activity_at_ms"].as_u64())
                            .is_some_and(|at| now_ms().saturating_sub(at) > ttl)
                    })
                {
                    let advanced = qualifying_activity(&candidate).is_some_and(|(revision, at)| {
                        now_ms().saturating_sub(at)
                            <= plugin.auto_listen.recent_window_ms.unwrap_or(900_000)
                            && prior
                                .as_ref()
                                .and_then(|r| activity_revision(r, "last_activity_revision"))
                                .is_none_or(|old| revision > old)
                    });
                    if !advanced {
                        continue;
                    }
                }
                if plugin.auto_listen.mode == AutoListenMode::Activity && !manual {
                    let fresh = qualifying_activity(&candidate).is_some_and(|(revision, at)| {
                        now_ms().saturating_sub(at)
                            <= plugin.auto_listen.recent_window_ms.unwrap_or(900_000)
                            && prior
                                .as_ref()
                                .and_then(|r| activity_revision(r, "retired_activity_revision"))
                                .is_none_or(|old| revision > old)
                    });
                    let unexpired = restoring
                        && prior
                            .as_ref()
                            .and_then(|r| r["last_activity_at_ms"].as_u64())
                            .is_some_and(|at| {
                                plugin
                                    .auto_listen
                                    .idle_timeout_ms
                                    .is_none_or(|ttl| now_ms().saturating_sub(at) <= ttl)
                            });
                    if !fresh && !unexpired {
                        continue;
                    }
                }
                let args = json!({"about":candidate["title"],"discovered_at":candidate["discovered_at"],"_auto":true,"_restore":restoring});
                {
                    let mut args = args;
                    let mut caller = candidate.clone();
                    caller["title"] = json!(mcp::default_session_title(
                        &candidate,
                        &self.config.home_dir,
                        &plugin.display_name,
                        string(&slot.identity, "owner_user"),
                        string(&args, "about")
                    ));
                    if slot.local.is_none() {
                        args["_expected_revision"] = candidate["discovery_revision"].clone();
                        remote.entry(hub.clone()).or_default().push((args, caller));
                    } else {
                        let lock = self.state.registration_lock(hub)?;
                        let Some(_guard) = try_registration(&lock)? else {
                            continue;
                        };
                        match super::registration::register_endpoint_locked(
                            &self.state,
                            slot,
                            &self.host_id,
                            &args,
                            &caller,
                        ) {
                            Ok(_) => {
                                Self::reconcile_unlistened_locked(
                                    &self.state,
                                    slot,
                                    &self.host_id,
                                )?;
                                let _ = slot.directory();
                            }
                            Err(error) if error.starts_with("STALE_DISCOVERY") => {
                                stale.insert(harness.to_owned());
                            }
                            Err(error) => eprintln!("cito auto-listen {hub}: {error}"),
                        }
                    }
                }
            }
        }
        for (hub, jobs) in remote {
            let key = format!("auto-agent:{hub}");
            if self.receivers.get(&key).is_some_and(|w| !w.is_finished()) {
                continue;
            }
            let slot = self.slot(&hub)?.clone();
            let state = self.state.clone();
            let host = self.host_id.clone();
            self.receivers.insert(
                key,
                thread::spawn(move || {
                    for (args, caller) in jobs {
                        if slot.closed.load(Ordering::SeqCst) {
                            break;
                        }
                        if let Err(error) = super::registration::register_endpoint(
                            &state, &slot, &host, &args, &caller,
                        )
                        .map(|result| {
                            if let Some(warning) = result["warning"].as_str() {
                                eprintln!("cito auto-listen {hub}: {warning}");
                            }
                        }) {
                            if error.starts_with("STALE_DISCOVERY") {
                                let _ = state.invalidate_discovery(string(&caller, "harness"));
                            } else {
                                eprintln!("cito auto-listen {hub}: {error}");
                            }
                        }
                    }
                }),
            );
        }
        if let Some(activity) = self.session_activity.as_mut() {
            for harness in stale {
                activity.mark_stale(&harness);
            }
        }
        Ok(())
    }

    /// Give fast providers a bounded first-render opportunity. Workers retain
    /// ownership after the deadline and are reaped by the regular poll path.
    fn update_discovery_demand(&mut self) {
        let configured: HashSet<String> = self
            .plugins
            .agent_plugins()
            .into_iter()
            .filter(|plugin| {
                self.setup_inspections
                    .get(&plugin.id)
                    .is_some_and(|e| e.worker.is_none())
                    && self
                        .inspect_setup(plugin)
                        .is_ok_and(|(_, configured, _)| configured)
            })
            .map(|plugin| plugin.id.clone())
            .collect();
        let background = self.menu_open
            || self.directory_open
            || self.active_settings_hub.is_some()
            || self
                .slots
                .iter()
                .any(|(hub, slot)| slot.config["autoListen"] == true || self.state.db.query(
                    "SELECT harness FROM local_listeners WHERE hub=? AND status='UNLISTENED' AND user_stopped=0 AND listen_mode='manual' AND retirement_reason='hard_exit'",
                    &[json!(hub)],
                ).is_ok_and(|rows| rows.iter().any(|row| configured.contains(string(row, "harness")))));
        if let Some(activity) = self.session_activity.as_mut() {
            activity.discovery_demand(background, configured);
        }
    }

    fn set_ui(&mut self, mailbox: Arc<UiMailbox>) {
        self.disconnect_ui();
        self.ui_generation += 1;
        self.ui = Some(mailbox);
        if let Ok(mut previous) = self.last_ui_revision.lock() {
            *previous = None;
        }
        if let Ok(mut previous) = self.last_ui_frame.lock() {
            *previous = None;
        }
        self.push_ui();
    }

    fn validate_ui_generation(&self, generation: Option<u64>) -> Result<(), String> {
        if generation.is_some_and(|generation| generation != self.ui_generation) {
            return Err("UI connection replaced".into());
        }
        Ok(())
    }

    fn disconnect_ui(&mut self) {
        self.ui_generation += 1;
        if let Some(mailbox) = self.ui.take() {
            mailbox.close();
        }
        self.menu_open = false;
        self.directory_open = false;
        self.active_settings_hub = None;
        self.update_discovery_demand();
    }

    fn refresh_available(&mut self) -> Result<(), String> {
        self.refresh_available_until(Instant::now() + Duration::from_millis(1500))
    }

    fn refresh_initial_setup_until(&mut self, deadline: Instant) {
        // A first snapshot shares its discovery deadline with the initial inspection.
        let completion = self.setup_completion.clone();
        loop {
            let before = *completion.0.lock().unwrap_or_else(|e| e.into_inner());
            self.refresh_setup_inspections();
            if !self
                .setup_inspections
                .values()
                .any(|e| e.generation == 0 && e.worker.is_some())
                || Instant::now() >= deadline
                || shutdown_requested()
            {
                return;
            }
            let revision = completion.0.lock().unwrap_or_else(|e| e.into_inner());
            let _ = completion
                .1
                .wait_timeout_while(
                    revision,
                    deadline.saturating_duration_since(Instant::now()),
                    |v| *v == before,
                )
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    fn refresh_available_until(&mut self, deadline: Instant) -> Result<(), String> {
        self.refresh_initial_setup_until(deadline);
        self.update_discovery_demand();
        let live = self.live_receivers()?;
        if let Some(activity) = self.session_activity.as_mut() {
            activity.refresh_discovery();
            loop {
                let observed = activity.completion_revision();
                activity.tick(&self.access.state, &live)?;
                if !activity.discovery_pending()
                    || Instant::now() >= deadline
                    || shutdown_requested()
                {
                    break;
                }
                activity.wait_for_completion(observed, deadline);
            }
        }
        self.auto_listen()
    }
    fn mutate_config(
        &self,
        action: impl FnOnce(&mut Value) -> Result<(), String>,
    ) -> Result<(), String> {
        if let Some(parent) = self.config.config_path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let target = crate::support::publication_target(&self.config.config_path)?;
        crate::plugins::with_config_lock(&target, || {
            let mut raw = match fs::read_to_string(&target) {
                Ok(text) => serde_json::from_str(&text)
                    .map_err(|e| format!("INVALID_INPUT: Invalid config: {e}"))?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
                Err(error) => return Err(error.to_string()),
            };
            if !raw.is_object() {
                return Err("INVALID_INPUT: Config must be an object".into());
            }
            action(&mut raw)?;
            let rendered = serde_json::to_string_pretty(&raw).map_err(|e| e.to_string())?;
            let (_, hubs, _) = crate::plugins::try_normalize_existing_config(Some(&rendered))
                .map_err(|error| format!("INVALID_INPUT: {error}"))?;
            for key in ["defaultHub", "default_hub"] {
                if raw
                    .get(key)
                    .is_some_and(|value| value.as_str().is_none_or(|name| !hubs.contains_key(name)))
                {
                    return Err("INVALID_INPUT: Invalid defaultHub".into());
                }
            }
            for cfg in hubs.values().chain(std::iter::once(&raw)) {
                if let Some(user) = cfg.get("user").filter(|user| !user.is_null()) {
                    if !user.as_str().is_some_and(crate::plugins::validate_user) {
                        return Err("INVALID_INPUT: User must contain 1-64 ASCII letters, numbers, dots, underscores or hyphens".into());
                    }
                }
            }
            crate::support::atomic_write_with_mode(&target, rendered.as_bytes(), Some(0o600))?;
            Ok(())
        })
    }
    fn cancel_logins(&self) {
        // Wake callback and lock waits before joining workers. Do not acquire
        // publication locks here: login workers release them before re-entering service.
        for attempt in self.hub_controls.values().filter_map(|c| c.login.as_ref()) {
            attempt.cancelled.store(true, Ordering::SeqCst);
        }
        for pending in self
            .hub_controls
            .values()
            .filter_map(|c| c.recovery.as_ref())
            .chain(self.retired_auth.iter())
        {
            pending.cancelled.store(true, Ordering::SeqCst);
        }
    }

    fn join_recoveries(&mut self) {
        for pending in self
            .hub_controls
            .values_mut()
            .filter_map(|c| c.recovery.take())
            .chain(self.retired_auth.drain(..))
        {
            let _ = pending.worker.join();
        }
    }

    fn shutdown(&mut self) {
        self.closing = true;
        self.cancel_logins();
        for entry in self.setup_inspections.values_mut() {
            if let Some((_, worker)) = entry.worker.take() {
                let _ = worker.join();
            }
        }
        self.join_recoveries();
        for slot in self.slots.values() {
            let _ = slot.close_and_drain();
        }
        for (_, receiver) in self.receivers.drain() {
            let _ = receiver.join();
        }
        for (_, probe) in self.probes.drain() {
            if let Some(probe) = probe.worker {
                let _ = probe.join();
            }
        }
        self.host_server.take();
        self.local.shutdown();
        if let Some(ui) = self.ui.take() {
            ui.close();
        }
        for event in self.events.drain(..) {
            let _ = event.join();
        }
        let _ = self.state.db.execute(
            "DELETE FROM daemon_owner WHERE pid=?",
            &[json!(std::process::id())],
        );
    }
}
