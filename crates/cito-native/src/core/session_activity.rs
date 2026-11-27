//! Cached discovery and serialized, endpoint-local badge effects. No worker owns a DB lock.
use super::{identity::string, state::LocalState};
use crate::plugins::{
    now_ms, plugin_request_with_progress, selected_plugin_dir, LoadedPlugins, NativeConfig,
    PluginDescriptor,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const DISCOVERY_TIMEOUT: Duration = Duration::from_millis(1500);
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const BADGE_TIMEOUT: Duration = Duration::from_millis(1500);
const BADGE_RETRY: Duration = Duration::from_millis(250);
const MAX_WORKERS: usize = 4;
const INACTIVE_FAILURE_LIMIT: u32 = 7;
fn badge_retry(failures: u32) -> Duration {
    (BADGE_RETRY * (1u32 << failures.saturating_sub(1).min(6))).min(Duration::from_secs(15))
}

type Worker<T> = JoinHandle<Result<T, String>>;

struct Discovery {
    plugin: PluginDescriptor,
    cached: Vec<Value>,
    stale: bool,
    refreshed_at: u64,
    revision: Option<Arc<AtomicU64>>,
    requested_generation: u64,
    cancelled: Arc<AtomicBool>,
    error: Option<String>,
    seen_revision: u64,
    refreshed_revision: u64,
    due: Instant,
    flight: DiscoveryFlight,
}

struct DiscoveryFlight {
    started_at: u64,
    started_revision: u64,
    started_generation: u64,
    worker: Option<Worker<Vec<Value>>>,
    complete: Arc<AtomicBool>,
}

/// A PID addresses one endpoint even when it changes sessions. Without a PID the
/// plugin addresses the session itself. Both cases serialize old and new intent.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Endpoint {
    Pid(u64),
    Session(String),
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Target {
    harness: String,
    endpoint: Endpoint,
}
impl Target {
    fn of(row: &Value) -> Self {
        Self {
            harness: string(row, "harness").to_string(),
            endpoint: match endpoint_pid(row) {
                Some(pid) => Endpoint::Pid(pid),
                None => Endpoint::Session(string(row, "session_id").to_string()),
            },
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Intent {
    session_id: String,
    endpoint_pid: Option<u64>,
    badge: Option<String>,
}
struct Badge {
    plugin: PluginDescriptor,
    cancelled: Arc<AtomicBool>,
    desired: Intent,
    sent: Option<Intent>,
    retry_at: Instant,
    worker: Option<Worker<Intent>>,
    attempted: Option<Intent>,
    failures: u32,
}

/// Call `tick` after receiver reconciliation and after admission/ack/unlisten/
/// restore/switch changes. `live_active` must contain only rows with a live client
/// and a current, non-cancelled receiver (including its owner and generation).
/// Also tick periodically for expiry, retry, and discovery refresh. Four requests
/// per class may run concurrently; each provider has a 1500 ms discovery deadline.
pub struct SessionActivity {
    state_dir: std::path::PathBuf,
    refresh_interval: Duration,
    env: Value,
    discovery: Vec<Discovery>,
    badges: BTreeMap<Target, Badge>,
    badge_inputs: Option<(i32, Vec<Value>, Instant)>,
    stopping: Arc<AtomicBool>,
    closed: bool,
    background_discovery: bool,
    configured: Option<HashSet<String>>,
    completion: Arc<(std::sync::Mutex<u64>, std::sync::Condvar)>,
}
pub(super) struct CompletionNotice {
    pub(super) done: Arc<AtomicBool>,
    pub(super) completion: Arc<(std::sync::Mutex<u64>, std::sync::Condvar)>,
}
impl Drop for CompletionNotice {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        let mut version = self
            .completion
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *version = version.wrapping_add(1);
        self.completion.1.notify_all();
        super::daemon::wake_daemon();
    }
}
impl SessionActivity {
    pub fn new(config: &NativeConfig, plugins: &LoadedPlugins) -> Self {
        Self::with_refresh_interval(config, plugins, REFRESH_INTERVAL)
    }
    fn with_refresh_interval(
        config: &NativeConfig,
        plugins: &LoadedPlugins,
        refresh_interval: Duration,
    ) -> Self {
        Self {
            refresh_interval,
            state_dir: config.state_dir.clone(),
            env: json!({"HOME":config.home_dir,"CITO_STATE_DIR":config.state_dir,
                "CITO_CONFIG_PATH":config.config_path}),
            discovery: plugins
                .agent_plugins()
                .into_iter()
                .filter(|p| !plugins.rejected_ids.contains(&p.id))
                .map(|p| Discovery {
                    plugin: p.clone(),
                    cached: Vec::new(),
                    stale: false,
                    refreshed_at: 0,
                    revision: None,
                    requested_generation: 0,
                    cancelled: Arc::new(AtomicBool::new(false)),
                    error: None,
                    seen_revision: 0,
                    refreshed_revision: 0,
                    due: Instant::now(),
                    flight: DiscoveryFlight {
                        started_at: 0,
                        started_revision: 0,
                        started_generation: 0,
                        worker: None,
                        complete: Arc::new(AtomicBool::new(false)),
                    },
                })
                .collect(),
            badges: BTreeMap::new(),
            badge_inputs: None,
            stopping: Arc::new(AtomicBool::new(false)),
            closed: false,
            background_discovery: true,
            configured: None,
            completion: Arc::new((std::sync::Mutex::new(0), std::sync::Condvar::new())),
        }
    }

    /// Install current descriptor values without losing unrelated in-flight work.
    /// Removed providers are drained outside the service lock by the caller.
    pub(super) fn reload(
        &mut self,
        config: &NativeConfig,
        plugins: &LoadedPlugins,
        changed: &HashSet<String>,
    ) -> Self {
        let mut retired = Self::new(config, plugins);
        let next = std::mem::take(&mut retired.discovery);
        for fresh in next {
            let id = &fresh.plugin.id;
            if let Some(index) = self.discovery.iter().position(|p| &p.plugin.id == id) {
                if changed.contains(id) {
                    self.remove_candidates(id);
                    let provider = &mut self.discovery[index];
                    provider.cancelled.store(true, Ordering::Release);
                    provider.cancelled = fresh.cancelled.clone();
                    provider.requested_generation = provider.requested_generation.wrapping_add(1);
                    for (target, badge) in &mut self.badges {
                        if &target.harness == id {
                            badge.plugin = fresh.plugin.clone();
                            badge.cancelled = fresh.cancelled.clone();
                            badge.sent = None;
                            badge.attempted = None;
                            badge.failures = 0;
                            badge.retry_at = Instant::now();
                        }
                    }
                }
                self.discovery[index].plugin = fresh.plugin;
            } else {
                self.discovery.push(fresh);
            }
        }
        let active: HashSet<_> = plugins
            .agent_plugins()
            .into_iter()
            .filter(|p| !plugins.rejected_ids.contains(&p.id))
            .map(|p| &p.id)
            .collect();
        let (kept, removed) = std::mem::take(&mut self.discovery)
            .into_iter()
            .partition(|p| active.contains(&p.plugin.id));
        self.discovery = kept;
        retired.discovery = removed;
        for p in &retired.discovery {
            p.cancelled.store(true, Ordering::Release);
            if let Some(revision) = &p.revision {
                revision.fetch_add(1, Ordering::SeqCst);
            }
        }
        retired.badges = self
            .badges
            .extract_if(.., |target, _| !active.contains(&target.harness))
            .collect();
        if !changed.is_empty() {
            self.badge_inputs = None;
        }
        retired
    }

    /// Targeted probes observe endpoint-local state even with discovery disabled.
    pub(super) fn observe_badge(&mut self, observation: &Value) {
        self.reap_badges();
        let Some(observed) = observation.get("badge") else {
            return;
        };
        if let Some(badge) = self.badges.get_mut(&Target::of(observation)) {
            if badge.desired.session_id == string(observation, "session_id")
                && *observed != json!(badge.desired.badge)
            {
                badge.sent = None;
                badge.retry_at = Instant::now();
            }
        }
    }

    pub fn discovery_demand(&mut self, background: bool, configured: HashSet<String>) {
        self.background_discovery = background;
        self.configured = Some(configured);
    }
    pub fn completion_revision(&self) -> u64 {
        self.completion.0.lock().map(|v| *v).unwrap_or(0)
    }
    pub fn wait_for_completion(&self, observed: u64, deadline: Instant) {
        if let Ok(version) = self.completion.0.lock() {
            let _ = self.completion.1.wait_timeout_while(
                version,
                deadline.saturating_duration_since(Instant::now()),
                |v| *v == observed,
            );
        }
    }

    /// Mark discovery due without spawning, waiting, or duplicating in-flight work.
    pub fn refresh_discovery(&mut self) {
        for provider in &mut self.discovery {
            provider.requested_generation = provider.requested_generation.wrapping_add(1);
            provider.due = Instant::now();
        }
    }
    pub fn remove_candidates(&mut self, harness: &str) {
        self.mark_stale(harness);
        for provider in self.discovery.iter_mut().filter(|p| p.plugin.id == harness) {
            provider.cached.clear();
            provider.error = None;
            provider.refreshed_at = 0;
        }
    }
    pub fn discovery_status(&self) -> Vec<Value> {
        self.discovery
            .iter()
            .map(|p| {
                let status = if p.error.is_some() {
                    "error"
                } else if p.flight.worker.is_some() && p.refreshed_at == 0 {
                    "scanning"
                } else {
                    "ready"
                };
                json!({"harness":p.plugin.id,"status":status,"error":p.error})
            })
            .collect()
    }

    /// Revoke cached enrollment authority while preserving presentation and workers.
    pub fn mark_stale(&mut self, harness: &str) {
        for provider in self.discovery.iter_mut().filter(|p| p.plugin.id == harness) {
            if let Some(revision) = &provider.revision {
                revision.fetch_add(1, Ordering::SeqCst);
            }
            provider.stale = true;
            provider.due = Instant::now();
        }
    }

    pub fn discovery_pending(&self) -> bool {
        self.discovery
            .iter()
            .any(|provider| provider.flight.worker.is_some())
    }

    /// Freshly confirmed endpoints eligible for automatic enrollment. Retained
    /// inventories remain visible through available(), but cannot establish identity.
    pub fn candidates(&self) -> impl Iterator<Item = Value> + '_ {
        self.discovery
            .iter()
            .filter(|provider| {
                !provider.stale
                    && provider.flight.worker.is_none()
                    && provider.revision.as_ref().is_none_or(|revision| {
                        revision.load(Ordering::SeqCst) == provider.refreshed_revision
                    })
            })
            .flat_map(|provider| {
                provider.cached.iter().map(move |candidate| {
                    let mut candidate = candidate.clone();
                    candidate["discovered_at"] = json!(provider.refreshed_at);
                    candidate["discovery_revision"] = json!(provider.refreshed_revision);
                    candidate
                })
            })
    }

    /// Read the cache only. Refilter against current live rows so a just-listened
    /// session disappears immediately, even while a provider is still running.
    pub fn available(&self, live_active: &[Value], total_hubs: usize) -> Vec<Value> {
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for provider in &self.discovery {
            let mut group = Vec::new();
            for candidate in &provider.cached {
                let mut hubs = Vec::new();
                let mut unscoped = false;
                for row in live_active.iter().filter(|row| {
                    string(row, "harness") == provider.plugin.id
                        && (row["session_id"] == candidate["session_id"]
                            || endpoint_pid(candidate)
                                .is_some_and(|pid| endpoint_pid(row) == Some(pid)))
                }) {
                    if let Some(hub) = row["hub"].as_str() {
                        if !hubs.iter().any(|h| h == hub) {
                            hubs.push(hub.to_string());
                        }
                    } else {
                        unscoped = true;
                    }
                }
                if unscoped || hubs.len() >= total_hubs.max(1) {
                    continue;
                }
                if !seen.insert((
                    provider.plugin.id.clone(),
                    string(candidate, "session_id").to_string(),
                    endpoint_pid(candidate),
                )) {
                    continue;
                }
                let mut candidate = candidate.clone();
                if let Some(object) = candidate.as_object_mut() {
                    object.remove("listening_hubs");
                    if !hubs.is_empty() {
                        object.insert("listening_hubs".into(), json!(hubs));
                    }
                }
                group.push(candidate);
            }
            if group.iter().any(|row| endpoint_pid(row).is_some()) {
                group.sort_by(|a, b| {
                    b["updated_at_ms"]
                        .as_u64()
                        .unwrap_or(0)
                        .cmp(&a["updated_at_ms"].as_u64().unwrap_or(0))
                        .then_with(|| {
                            endpoint_pid(b)
                                .unwrap_or(0)
                                .cmp(&endpoint_pid(a).unwrap_or(0))
                        })
                });
            }
            result.extend(group.into_iter().take(8));
        }
        result
    }

    /// Returns whether a completed discovery changed the cache. DB reads and
    /// thread admission are synchronous; all plugin requests run on owned workers.
    pub fn tick(&mut self, state: &LocalState, live_active: &[Value]) -> Result<bool, String> {
        if self.closed {
            return Ok(false);
        }
        for provider in &mut self.discovery {
            if provider.revision.is_none() {
                provider.revision = Some(state.discovery_revision(&provider.plugin.id)?);
            }
            let revision = provider
                .revision
                .as_ref()
                .map(|revision| revision.load(Ordering::SeqCst))
                .unwrap_or(0);
            if revision != provider.seen_revision {
                provider.seen_revision = revision;
                provider.stale = true;
                provider.due = Instant::now();
            }
        }
        let changed = self.reap_discovery();
        self.reap_badges();
        // Retain expiry reconciliation even without local writes; otherwise an
        // idle, closed UI must not rescan listener history every second.
        let revision = state.db.mutation_revision()?;
        let reconcile = changed
            || self.background_discovery
            || self
                .badge_inputs
                .as_ref()
                .is_none_or(|(previous, live, at)| {
                    *previous != revision
                        || live != live_active
                        || at.elapsed() >= self.refresh_interval
                });
        if reconcile {
            for provider in &mut self.discovery {
                for candidate in &mut provider.cached {
                    if let Some(title) =
                        state.custom_title(&provider.plugin.id, string(candidate, "session_id"))?
                    {
                        candidate["title"] = json!(title);
                        candidate["custom_title"] = json!(true);
                    }
                }
            }
            // Persisted rows identify endpoints to clear. Only live agent receivers
            // contribute unread signals to an endpoint's badge.
            let mut desired = desired_badges(state, live_active)?;
            // Historical endpoints need one bounded clear after startup, not a fresh
            // retry lifecycle on every DB scan once their badge entry has retired.
            if self.badge_inputs.is_some() {
                desired.retain(|target, intent| {
                    intent.badge.is_some() || self.badges.contains_key(target)
                });
            }
            self.badge_inputs = Some((revision, live_active.to_vec(), Instant::now()));
            for (target, badge) in &mut self.badges {
                if !desired.contains_key(target) && badge.desired.badge.is_some() {
                    badge.desired.badge = None;
                    badge.failures = 0;
                    badge.retry_at = Instant::now();
                }
            }
            for (target, intent) in desired {
                if let Some(badge) = self.badges.get_mut(&target) {
                    if changed
                        && self
                            .discovery
                            .iter()
                            .filter(|provider| provider.plugin.id == target.harness)
                            .flat_map(|provider| &provider.cached)
                            .any(|candidate| {
                                Target::of(candidate) == target
                                    && candidate["session_id"] == intent.session_id
                                    && candidate
                                        .get("badge")
                                        .is_some_and(|observed| *observed != json!(intent.badge))
                            })
                    {
                        badge.sent = None;
                        badge.retry_at = Instant::now();
                    }
                    if badge.desired != intent {
                        badge.desired = intent;
                        badge.failures = 0;
                        badge.retry_at = Instant::now();
                    }
                } else if let Some(provider) = self
                    .discovery
                    .iter()
                    .find(|p| p.plugin.id == target.harness)
                {
                    self.badges.insert(
                        target,
                        Badge {
                            plugin: provider.plugin.clone(),
                            cancelled: provider.cancelled.clone(),
                            desired: intent,
                            sent: None,
                            retry_at: Instant::now(),
                            worker: None,
                            attempted: None,
                            failures: 0,
                        },
                    );
                }
            }
        }
        self.badges.retain(|_, badge| {
            badge.worker.is_some()
                || badge.desired.badge.is_some()
                || (badge.sent.as_ref() != Some(&badge.desired)
                    && badge.failures < INACTIVE_FAILURE_LIMIT)
        });
        self.start_badges()?;
        let mut capacity = MAX_WORKERS.saturating_sub(
            self.discovery
                .iter()
                .filter(|p| p.flight.worker.is_some())
                .count(),
        );
        for provider in &mut self.discovery {
            if capacity == 0 {
                break;
            }
            if provider.flight.worker.is_some()
                || provider.due > Instant::now()
                || self
                    .configured
                    .as_ref()
                    .is_some_and(|ids| !ids.contains(&provider.plugin.id))
                || (!self.background_discovery
                    && provider.requested_generation == provider.flight.started_generation)
            {
                continue;
            }
            provider.flight.started_revision = provider
                .revision
                .as_ref()
                .map(|revision| revision.load(Ordering::SeqCst))
                .unwrap_or(0);
            provider.flight.started_at = now_ms();
            provider.flight.started_generation = provider.requested_generation;
            let plugin = provider.plugin.clone();
            // Restoration must not depend on a provider's recent/display slices.
            let retained: Vec<Value> = state.db.query(
                "SELECT DISTINCT session_id FROM local_listeners WHERE harness=? AND user_stopped=0 AND (status='ACTIVE' OR retirement_reason IN ('hard_exit','disconnected'))",
                &[json!(plugin.id)],
            )?.into_iter().map(|row| row["session_id"].clone()).collect();
            let request = json!({"op":"discover_sessions","state_dir":self.state_dir,"env":self.env,
                "retained_session_ids":retained,
                "recent_window_ms":plugin.auto_listen.recent_window_ms.unwrap_or(900_000),"plugin_dir":selected_plugin_dir(&self.state_dir, &plugin)});
            let stopping = self.stopping.clone();
            let cancelled = provider.cancelled.clone();
            provider.flight.complete.store(false, Ordering::Release);
            let finished = CompletionNotice {
                done: provider.flight.complete.clone(),
                completion: self.completion.clone(),
            };
            provider.flight.worker = Some(
                thread::Builder::new()
                    .name("cito-discovery".into())
                    .spawn(move || {
                        let _finished = finished;
                        let response = plugin_request_with_progress(
                            &plugin,
                            &request,
                            DISCOVERY_TIMEOUT,
                            |_| check_running(&stopping).and_then(|()| check_running(&cancelled)),
                        )?;
                        if response["ok"] != true {
                            return Err(crate::support::response_diagnostic(
                                &response,
                                "Session discovery failed",
                            ));
                        }
                        let rows = response["sessions"]
                            .as_array()
                            .ok_or("Invalid discovery sessions")?;
                        Ok(rows
                            .iter()
                            .filter(|row| {
                                row.is_object()
                                    && string(row, "harness") == plugin.id
                                    && !string(row, "session_id").is_empty()
                                    && !string(row, "cwd").trim().is_empty()
                            })
                            .cloned()
                            .map(|mut row| {
                                if row["needs_reload"] == true {
                                    if let Some(hint) = &plugin.reload_hint {
                                        row["reload_hint"] = json!(hint);
                                    }
                                }
                                row["title"] = json!(row["title"]
                                    .as_str()
                                    .filter(|s| !s.trim().is_empty())
                                    .unwrap_or(&plugin.display_name)
                                    .trim()
                                    .chars()
                                    .take(500)
                                    .collect::<String>());
                                row
                            })
                            .collect())
                    })
                    .map_err(|e| format!("Cannot start discovery worker: {e}"))?,
            );
            capacity -= 1;
        }
        Ok(changed)
    }

    fn reap_discovery(&mut self) -> bool {
        let mut changed = false;
        for provider in &mut self.discovery {
            if provider.flight.worker.is_none()
                || (!provider.flight.complete.load(Ordering::Acquire)
                    && !provider
                        .flight
                        .worker
                        .as_ref()
                        .is_some_and(|worker| worker.is_finished()))
            {
                continue;
            }
            if let Some(worker) = provider.flight.worker.take() {
                provider.error = None;
                changed = true;
                let completion = worker.join();
                if provider.revision.as_ref().is_some_and(|revision| {
                    revision.load(Ordering::SeqCst) != provider.flight.started_revision
                }) {
                    provider.stale = true;
                    provider.due = Instant::now();
                    continue;
                }
                let rows = match completion {
                    Ok(Ok(rows)) => Some(rows),
                    Ok(Err(error)) => {
                        eprintln!("cito discovery {}: {error}", provider.plugin.id);
                        provider.error = Some(error);
                        None
                    }
                    Err(_) => {
                        eprintln!("cito discovery {}: worker panicked", provider.plugin.id);
                        provider.error = Some("Discovery worker panicked".into());
                        None
                    }
                };
                provider.stale = rows.is_none()
                    || provider.revision.as_ref().is_some_and(|revision| {
                        revision.load(Ordering::SeqCst) != provider.flight.started_revision
                    });
                let invalidated = provider.requested_generation
                    != provider.flight.started_generation
                    || (provider.stale && rows.is_some());
                if rows.is_none() {
                    provider.cached.clear();
                }
                if let Some(rows) = rows {
                    // A refresh already in flight at revocation is not newer evidence,
                    // even if its result is joined after the revocation transaction.
                    provider.refreshed_at = provider.flight.started_at;
                    provider.refreshed_revision = provider.flight.started_revision;
                    if !provider.stale {
                        provider.cached = rows;
                    }
                }
                provider.due = Instant::now()
                    + if invalidated {
                        Duration::ZERO
                    } else {
                        self.refresh_interval
                    };
            }
        }
        changed
    }
    fn reap_badges(&mut self) {
        for badge in self.badges.values_mut() {
            if !badge.worker.as_ref().is_some_and(|w| w.is_finished()) {
                continue;
            }
            if let Some(worker) = badge.worker.take() {
                let completion = worker.join();
                // The sink was replaced during this call; only the new sink can acknowledge it.
                if badge.attempted.is_none() {
                    continue;
                }
                match completion {
                    Ok(Ok(sent)) => {
                        badge.sent = Some(sent);
                        badge.failures = 0;
                    }
                    result => {
                        // A failed call may already have applied an effect. Its
                        // previous acknowledged value cannot suppress a retry.
                        badge.sent = None;
                        if badge.attempted.as_ref() == Some(&badge.desired) {
                            badge.failures = badge.failures.saturating_add(1);
                            badge.retry_at = Instant::now() + badge_retry(badge.failures);
                            eprintln!("cito badge {}: {:?}", badge.plugin.id, result);
                        }
                        // An obsolete failed intent never delays its replacement.
                    }
                }
            }
        }
    }
    fn start_badges(&mut self) -> Result<(), String> {
        let mut capacity =
            MAX_WORKERS.saturating_sub(self.badges.values().filter(|b| b.worker.is_some()).count());
        for badge in self.badges.values_mut() {
            if capacity == 0 {
                break;
            }
            if badge.worker.is_some()
                || badge.sent.as_ref() == Some(&badge.desired)
                || badge.retry_at > Instant::now()
            {
                continue;
            }
            let plugin = badge.plugin.clone();
            let intent = badge.desired.clone();
            badge.attempted = Some(intent.clone());
            let request = badge_request(&self.state_dir, &self.env, &intent);
            let stopping = self.stopping.clone();
            let cancelled = badge.cancelled.clone();
            badge.worker = Some(
                thread::Builder::new()
                    .name("cito-badge".into())
                    .spawn(move || {
                        send_badge(&plugin, &request, |_| {
                            check_running(&stopping).and_then(|()| check_running(&cancelled))
                        })?;
                        Ok(intent)
                    })
                    .map_err(|e| format!("Cannot start badge worker: {e}"))?,
            );
            capacity -= 1;
        }
        Ok(())
    }

    /// Cancel and join every in-flight request, then clear each known endpoint
    /// once with bounded requests. Call after stopping receivers, outside the
    /// service lock; no DB access is needed. Idempotent; Drop provides a fallback.
    pub fn shutdown(&mut self) -> Result<(), String> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.stopping.store(true, Ordering::Release);
        for provider in &mut self.discovery {
            if let Some(worker) = provider.flight.worker.take() {
                let _ = worker.join();
            }
        }
        for badge in self.badges.values_mut() {
            if let Some(worker) = badge.worker.take() {
                badge.sent = worker
                    .join()
                    .ok()
                    .and_then(Result::ok)
                    .filter(|_| badge.attempted.is_some());
            }
        }
        let clears: Vec<_> = self
            .badges
            .values()
            .filter_map(|badge| {
                let mut intent = badge.desired.clone();
                intent.badge = None;
                (badge.sent.as_ref() != Some(&intent)).then(|| {
                    (
                        badge.plugin.clone(),
                        badge_request(&self.state_dir, &self.env, &intent),
                    )
                })
            })
            .collect();
        let mut errors = Vec::new();
        for chunk in clears.chunks(MAX_WORKERS) {
            thread::scope(|scope| {
                let mut workers = Vec::new();
                for (plugin, request) in chunk {
                    match thread::Builder::new()
                        .name("cito-badge-clear".into())
                        .spawn_scoped(scope, move || send_badge(plugin, request, |_| Ok(())))
                    {
                        Ok(worker) => workers.push(worker),
                        Err(error) => {
                            errors.push(format!("Cannot start badge clear worker: {error}"))
                        }
                    }
                }
                for worker in workers {
                    match worker.join() {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => errors.push(error),
                        Err(_) => errors.push("Badge shutdown worker panicked".into()),
                    }
                }
            });
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}
impl Drop for SessionActivity {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
fn check_running(stopping: &AtomicBool) -> Result<(), String> {
    if stopping.load(Ordering::Acquire) {
        Err("Session activity stopped".into())
    } else {
        Ok(())
    }
}
fn endpoint_pid(row: &Value) -> Option<u64> {
    row["endpoint_pid"].as_u64().filter(|pid| *pid > 0)
}
fn badge_request(state_dir: &std::path::Path, env: &Value, intent: &Intent) -> Value {
    json!({"op":"update_badge","endpoint_pid":intent.endpoint_pid,"session_id":intent.session_id,
        "badge":intent.badge,"state_dir":state_dir,"env":env})
}
fn send_badge(
    plugin: &PluginDescriptor,
    request: &Value,
    progress: impl FnMut(Duration) -> Result<(), String>,
) -> Result<(), String> {
    let response = plugin_request_with_progress(plugin, request, BADGE_TIMEOUT, progress)?;
    if response["ok"] == true {
        Ok(())
    } else {
        Err(crate::support::response_diagnostic(
            &response,
            "Session badge update failed",
        ))
    }
}

fn desired_badges(
    state: &LocalState,
    live_active: &[Value],
) -> Result<BTreeMap<Target, Intent>, String> {
    let rows = state.db.query("SELECT * FROM local_listeners WHERE harness != 'human' ORDER BY (status='ACTIVE') DESC,binding_generation DESC,hub ASC,listener_id ASC", &[])?;
    let is_live = |row: &Value| {
        row["status"] == "ACTIVE"
            && live_active.iter().any(|live| {
                [
                    "hub",
                    "listener_id",
                    "owner_token",
                    "binding_generation",
                    "harness",
                    "session_id",
                    "endpoint_pid",
                ]
                .iter()
                .all(|key| live[*key] == row[*key])
            })
    };
    let mut groups: BTreeMap<Target, Vec<&Value>> = BTreeMap::new();
    for row in &rows {
        groups.entry(Target::of(row)).or_default().push(row);
    }
    let mut desired = BTreeMap::new();
    for (key, rows) in groups {
        // Input ordering preserves the baseline's ACTIVE/generation/hub tie-break.
        // A live session outranks historical rows sharing a switched endpoint.
        let mut live = rows.iter().copied().filter(|row| is_live(row)).peekable();
        let (target, badge) = if let Some(target) = live.peek().copied() {
            let mut unread = 0;
            for row in live.filter(|row| row["session_id"] == target["session_id"]) {
                unread +=
                    state.unread_count(string(row, "hub"), string(row, "listener_id"), true)?;
            }
            (target, Some(format_badge(target, unread)))
        } else {
            (rows[0], None)
        };
        desired.insert(
            key,
            Intent {
                session_id: string(target, "session_id").into(),
                endpoint_pid: endpoint_pid(target),
                badge,
            },
        );
    }
    Ok(desired)
}

fn format_badge(row: &Value, unread: u64) -> String {
    let public = string(row, "public_id");
    // Both supported canonical suffix lengths use the same readable slug. The
    // original formatter removed its fixed 12-character suffix plus separator.
    let slug = if !public.is_empty() {
        public
            .rsplit_once('-')
            .map(|(slug, _)| slug.to_string())
            .unwrap_or_default()
    } else {
        let repo = string(row, "repo").rsplit('/').next().unwrap_or("");
        let cwd = std::path::Path::new(string(row, "cwd"))
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        [
            string(row, "tag"),
            repo,
            cwd,
            string(row, "title"),
            string(row, "harness"),
        ]
        .into_iter()
        .map(normalize_badge_slug)
        .find(|s| !s.is_empty())
        .unwrap_or_default()
    };
    let short: String = slug.chars().take(12).collect();
    let short = short.trim_end_matches('-');
    let short = if short.is_empty() {
        string(row, "harness")
    } else {
        short
    };
    if unread > 0 {
        format!("Cito · {short} ({unread})")
    } else {
        format!("Cito · {short}")
    }
}
fn normalize_badge_slug(raw: &str) -> String {
    let mut result = String::new();
    for ch in raw.chars() {
        if result.len() >= 20 {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            result.push(ch.to_ascii_lowercase());
        } else if !result.is_empty() && !result.ends_with('-') {
            result.push('-');
        }
    }
    result.trim_end_matches('-').into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    // Actual one-request NDJSON executable: the existing runner owns all pipes,
    // deadlines, process groups and cleanup. Options vary behavior, not the seam.
    const PROVIDER: &str = r#"
import json, os, pathlib, subprocess, sys, time
root = pathlib.Path(__file__).parent
opts = json.loads((root / 'options.json').read_text())
req = json.loads(sys.stdin.readline())
assert req['plugin_dir'] == str(root)
for key in ['HOME', 'CITO_STATE_DIR', 'CITO_CONFIG_PATH']:
    assert os.environ[key] == req['env'][key]
    assert str(root.parent) in os.environ[key]
def record(stage, **rest):
    with (root / 'calls.jsonl').open('a') as log:
        log.write(json.dumps(dict(stage=stage, pid=os.getpid(), at=time.time(), req=req, **rest)) + '\n')
record('start')
if req['op'] == 'update_badge':
    previous = root / ('active-pid-' + str(req.get('endpoint_pid') or req['session_id']))
    if previous.exists():
        try:
            os.kill(int(previous.read_text()), 0)
        except ProcessLookupError:
            pass
        else:
            record('overlap')
            raise RuntimeError('overlapping badge requests')
    previous.write_text(str(os.getpid()))
if opts.get('descendant') and req['op'] == 'discover_sessions':
    child = subprocess.Popen(['/bin/sleep', '30'])
    record('child', child_pid=child.pid)
time.sleep(opts.get('discovery_delay' if req['op'] == 'discover_sessions' else 'badge_delay', 0))
if req['op'] == 'discover_sessions':
    response = dict(ok=True, sessions=opts.get('sessions', []))
    if opts.get('malformed'):
        print('invalid json', flush=True)
        sys.exit(0)
else:
    (root / 'effect.json').write_text(json.dumps(req))
    fail = opts.get('fail_once') and not (root / 'failed').exists()
    if fail:
        (root / 'failed').touch()
    response = dict(ok=not fail)
    previous.unlink(missing_ok=True)
response = opts.get('response', response)
record('end', response=response)
print(json.dumps(response), flush=True)
"#;

    struct Fixture {
        dir: PathBuf,
        config: NativeConfig,
        state: LocalState,
        plugins: LoadedPlugins,
    }
    impl Fixture {
        fn new() -> Result<Self, String> {
            let dir = std::env::temp_dir().join(format!(
                "cito-session-activity-{}",
                crate::plugins::random_uuid_v4()
            ));
            fs::create_dir_all(dir.join("home")).map_err(|e| e.to_string())?;
            fs::write(dir.join("config.json"), "{}").map_err(|e| e.to_string())?;
            let config = NativeConfig {
                state_dir: dir.join("state"),
                config_path: dir.join("config.json"),
                home_dir: dir.join("home"),
                daemon_port: 0,
                user: "alice".into(),
                default_hub: "local".into(),
            };
            let state = LocalState::open(&config.state_dir)?;
            Ok(Self {
                dir,
                config,
                state,
                plugins: LoadedPlugins {
                    plugins: Vec::new(),
                    rejected_ids: HashSet::new(),
                },
            })
        }
        fn provider(&mut self, id: &str, options: Value) -> Result<(), String> {
            let path = self.dir.join(id);
            fs::create_dir_all(&path).map_err(|e| e.to_string())?;
            fs::write(path.join("provider.py"), PROVIDER).map_err(|e| e.to_string())?;
            fs::write(path.join("options.json"), options.to_string()).map_err(|e| e.to_string())?;
            self.plugins.plugins.push(crate::plugins::parse_manifest(
                &path,
                false,
                &json!({
                    "id":id,"kind":"agent","displayName":id,"sfSymbol":"bolt","tint":"blue",
                    "reloadHint":options["reloadHint"],
                    "command":["/usr/bin/env", format!("HOME={}", self.config.home_dir.display()),
                        format!("CITO_STATE_DIR={}", self.config.state_dir.display()),
                        format!("CITO_CONFIG_PATH={}", self.config.config_path.display()),
                        "/usr/bin/python3",path.join("provider.py")],
                }),
            )?);
            Ok(())
        }
        fn records(&self, id: &str) -> Vec<Value> {
            fs::read_to_string(self.dir.join(id).join("calls.jsonl"))
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect()
        }
        fn calls(&self, id: &str, op: &str) -> Vec<Value> {
            self.records(id)
                .into_iter()
                .filter(|row| row["stage"] == "start" && row["req"]["op"] == op)
                .collect()
        }
        fn effect(&self, id: &str) -> Option<Value> {
            fs::read_to_string(self.dir.join(id).join("effect.json"))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
        }
        fn listener(
            &self,
            hub: &str,
            session: &str,
            pid: Option<u64>,
            generation: u64,
        ) -> Result<Value, String> {
            self.state.db.execute("INSERT INTO local_listeners(hub,listener_id,public_id,principal_id,owner_user,host_id,host_name,harness,session_id,endpoint_pid,cwd,title,about,scope,status,binding_generation,owner_token,hub_url) VALUES(?,'c_abcdefghijkl','abcdefghijk--more-abcdefghijkl','none#alice','alice','host','host','agent',?,?,'/workspace','Target','','workspace','ACTIVE',?,'owner','local://')",&[json!(hub),json!(session),json!(pid),json!(generation)])?;
            self.state
                .listener(hub, "c_abcdefghijkl")?
                .ok_or_else(|| "Missing listener".into())
        }
        fn seed(
            &self,
            hub: &str,
            id: &str,
            state: &str,
            acked: Value,
            expires: u64,
        ) -> Result<(), String> {
            self.state.db.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state,acknowledged_at_ms,hub_url,principal_id) VALUES(?,?,'c_abcdefghijkl',1,1,'bob','remote','Sender','hello','2026-01-01T00:00:00.000Z',?,?,?,'local://','none#alice')",&[json!(hub),json!(id),json!(expires),json!(state),acked])?;
            Ok(())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
    fn pump(
        activity: &mut SessionActivity,
        f: &Fixture,
        live: &[Value],
        ready: impl Fn(&SessionActivity) -> bool,
    ) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            activity.tick(&f.state, live)?;
            if ready(activity) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("Session activity test timed out".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn alive(pid: u64) -> bool {
        // SAFETY: signal zero only checks the fixture-owned process's existence.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    fn assert_reaped(f: &Fixture) {
        let pids: HashSet<_> = f
            .plugins
            .plugins
            .iter()
            .flat_map(|p| f.records(&p.id))
            .flat_map(|row| [row["pid"].as_u64(), row["child_pid"].as_u64()])
            .flatten()
            .collect();
        let deadline = Instant::now() + Duration::from_secs(2);
        while pids.iter().any(|pid| alive(*pid)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            pids.iter().all(|pid| !alive(*pid)),
            "Fixture process survived shutdown: {pids:?}"
        );
    }

    #[test]
    fn paused_badges_skip_idle_sql_but_reconcile_mutations_liveness_and_expiry(
    ) -> Result<(), String> {
        let f = Fixture::new()?;
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        activity.discovery_demand(false, HashSet::new());
        activity.tick(&f.state, &[])?;
        // DDL does not change SQLite's row-mutation counter. A scan now would
        // fail, making this sensitive to accidental unconditional reconciliation.
        f.state.db.execute(
            "ALTER TABLE local_listeners RENAME TO hidden_listeners",
            &[],
        )?;
        for _ in 0..5 {
            activity.tick(&f.state, &[])?;
        }
        f.state
            .db
            .execute("INSERT INTO daemon_owner VALUES(1,42,1234)", &[])?;
        assert!(
            activity.tick(&f.state, &[]).is_err(),
            "A local mutation must request reconciliation"
        );
        f.state.db.execute(
            "ALTER TABLE hidden_listeners RENAME TO local_listeners",
            &[],
        )?;
        activity.tick(&f.state, &[])?;
        f.state.db.execute(
            "ALTER TABLE local_listeners RENAME TO hidden_listeners",
            &[],
        )?;
        assert!(
            activity
                .tick(&f.state, &[json!({"listener_id":"new"})])
                .is_err(),
            "Liveness changes must request reconciliation"
        );

        f.state.db.execute(
            "ALTER TABLE hidden_listeners RENAME TO local_listeners",
            &[],
        )?;
        let mut expiring =
            SessionActivity::with_refresh_interval(&f.config, &f.plugins, Duration::ZERO);
        expiring.discovery_demand(false, HashSet::new());
        expiring.tick(&f.state, &[])?;
        f.state.db.execute(
            "ALTER TABLE local_listeners RENAME TO hidden_listeners",
            &[],
        )?;
        assert!(
            expiring.tick(&f.state, &[]).is_err(),
            "Expiry requires fallback reconciliation without mutation or liveness changes"
        );
        Ok(())
    }

    #[test]
    fn discovery_projects_manifest_reload_hint_and_preserves_legacy_copy() -> Result<(), String> {
        let mut f = Fixture::new()?;
        f.provider("agent", json!({"reloadHint":"Reload the integration", "sessions":[
            {"harness":"agent","cwd":"/repo","session_id":"pending","needs_reload":true,"reload_hint":"Provider-local copy"},
            {"harness":"agent","cwd":"/repo","session_id":"ready","needs_reload":false}
        ]}))?;
        f.provider("legacy", json!({"sessions":[
            {"harness":"legacy","cwd":"/repo","session_id":"legacy","needs_reload":true,"reload_hint":"Legacy reload instructions"}
        ]}))?;
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        pump(&mut activity, &f, &[], |a| a.candidates().count() == 3)?;
        let hint = |id| {
            activity
                .candidates()
                .find(|row| row["session_id"] == id)
                .unwrap()["reload_hint"]
                .clone()
        };
        assert_eq!(hint("pending"), "Reload the integration");
        assert_eq!(hint("ready"), Value::Null);
        assert_eq!(hint("legacy"), "Legacy reload instructions");
        activity.shutdown()?;
        Ok(())
    }

    #[test]
    fn discovery_badge_observation_repairs_a_reloaded_endpoint() -> Result<(), String> {
        let mut f = Fixture::new()?;
        let candidate = json!({"harness":"agent","session_id":"s","cwd":"/repo","endpoint_pid":42,"badge":"Cito · abcdefghijk"});
        f.provider("agent", json!({"sessions":[candidate]}))?;
        let live = vec![f.listener("local", "s", Some(42), 1)?];
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        pump(&mut activity, &f, &live, |a| {
            !a.discovery_pending()
                && a.badges
                    .values()
                    .all(|badge| badge.sent.as_ref() == Some(&badge.desired))
        })?;
        let before = f.calls("agent", "update_badge").len();
        fs::write(f.dir.join("agent/options.json"),json!({"sessions":[{"harness":"agent","session_id":"s","cwd":"/repo","endpoint_pid":42,"badge":null}]}).to_string()).map_err(|e|e.to_string())?;
        activity.refresh_discovery();
        pump(&mut activity, &f, &live, |a| {
            !a.discovery_pending()
                && f.calls("agent", "update_badge").len() > before
                && a.badges
                    .values()
                    .all(|badge| badge.sent.as_ref() == Some(&badge.desired))
        })?;
        assert_eq!(f.effect("agent").unwrap()["badge"], "Cito · abcdefghijk");
        activity.shutdown()?;
        Ok(())
    }

    #[test]
    fn discovery_concurrent_deadline_failure_isolation_and_cache_only_read() -> Result<(), String> {
        let mut f = Fixture::new()?;
        f.provider(
            "fast",
            json!({"discovery_delay":0.2,"sessions":[{"harness":"fast","cwd":"/repo","session_id":"one"}]}),
        )?;
        f.provider("slow", json!({"discovery_delay":30,"descendant":true}))?;
        f.provider("invalid", json!({"malformed":true}))?;
        f.provider(
            "other",
            json!({"discovery_delay":0.2,"sessions":[{"harness":"other","cwd":"/repo","session_id":"two"}]}),
        )?;
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        let started = Instant::now();
        activity.tick(&f.state, &[])?;
        assert!(started.elapsed() < Duration::from_millis(200));
        pump(&mut activity, &f, &[], |a| a.available(&[], 1).len() == 2).map_err(|e| {
            format!(
                "{e}; fast={:?}; other={:?}",
                f.records("fast"),
                f.records("other")
            )
        })?;
        assert!(
            started.elapsed() < Duration::from_millis(1400),
            "Fast providers waited for slow provider"
        );
        let read = Instant::now();
        for _ in 0..100 {
            assert_eq!(activity.available(&[], 1).len(), 2);
        }
        assert!(read.elapsed() < Duration::from_millis(100));
        pump(&mut activity, &f, &[], |a| {
            a.discovery.iter().all(|p| p.flight.worker.is_none())
        })?;
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(f.calls("fast", "discover_sessions").len(), 1);
        assert_eq!(f.calls("other", "discover_sessions").len(), 1);
        let fast_end = f
            .records("fast")
            .into_iter()
            .find(|r| r["stage"] == "end")
            .ok_or("Missing completion")?["at"]
            .as_f64()
            .ok_or("Missing time")?;
        let other_start = f.calls("other", "discover_sessions")[0]["at"]
            .as_f64()
            .ok_or("Missing time")?;
        assert!(other_start < fast_end, "Providers ran sequentially");
        activity.shutdown()?;
        assert_reaped(&f);
        Ok(())
    }

    #[test]
    fn discovery_status_only_scans_on_cold_start_and_preserves_errors_during_retry(
    ) -> Result<(), String> {
        let mut f = Fixture::new()?;
        f.provider(
            "agent",
            json!({"response":{"ok":false,"message":"Provider unavailable"}}),
        )?;
        let mut activity = SessionActivity::with_refresh_interval(
            &f.config,
            &f.plugins,
            Duration::from_millis(100),
        );
        activity.tick(&f.state, &[])?;
        assert_eq!(activity.discovery[0].refreshed_at, 0);
        assert!(activity.discovery[0].flight.worker.is_some());
        assert_eq!(activity.discovery_status()[0]["status"], "scanning");
        pump(&mut activity, &f, &[], |a| !a.discovery_pending())?;
        assert_eq!(activity.discovery[0].refreshed_at, 0);
        assert_eq!(activity.discovery_status()[0]["status"], "error");
        let error = activity.discovery_status()[0]["error"].clone();
        assert_eq!(error, "Provider unavailable");

        let sessions =
            vec![json!({"harness":"agent","session_id":"one","cwd":"/repo","title":"Current"})];
        fs::write(
            f.dir.join("agent/options.json"),
            json!({"sessions":sessions}).to_string(),
        )
        .map_err(|e| e.to_string())?;
        activity.refresh_discovery();
        activity.tick(&f.state, &[])?;
        assert!(activity.discovery[0].flight.worker.is_some());
        assert_eq!(activity.discovery_status()[0]["status"], "error");
        assert_eq!(activity.discovery_status()[0]["error"], error);
        pump(&mut activity, &f, &[], |a| !a.discovery_pending())?;
        assert!(activity.discovery[0].refreshed_at > 0);
        assert_eq!(activity.discovery_status()[0]["status"], "ready");
        assert!(activity.discovery_status()[0]["error"].is_null());
        assert_eq!(activity.available(&[], 1), sessions);

        // Both explicit refresh and periodic discovery retain the visible inventory.
        for explicit in [true, false] {
            if explicit {
                activity.refresh_discovery();
            }
            pump(&mut activity, &f, &[], |a| a.discovery_pending())?;
            assert_eq!(activity.discovery_status()[0]["status"], "ready");
            assert_eq!(activity.discovery[0].cached, sessions);
            assert_eq!(activity.available(&[], 1), sessions);
            pump(&mut activity, &f, &[], |a| !a.discovery_pending())?;
            assert_eq!(activity.discovery_status()[0]["status"], "ready");
        }

        activity.remove_candidates("agent");
        assert_eq!(activity.discovery[0].refreshed_at, 0);
        assert!(activity.discovery[0].cached.is_empty());
        assert!(activity.discovery[0].error.is_none());
        activity.tick(&f.state, &[])?;
        assert!(activity.discovery[0].flight.worker.is_some());
        assert_eq!(activity.discovery_status()[0]["status"], "scanning");
        pump(&mut activity, &f, &[], |a| !a.discovery_pending())?;
        assert_eq!(activity.discovery_status()[0]["status"], "ready");
        assert_eq!(activity.available(&[], 1), sessions);
        activity.shutdown()?;
        Ok(())
    }

    #[test]
    fn reload_cancels_changed_discovery_without_interrupting_an_unrelated_provider(
    ) -> Result<(), String> {
        let mut f = Fixture::new()?;
        f.provider("changed", json!({"discovery_delay":1.2}))?;
        f.provider("unchanged", json!({"discovery_delay":0.4}))?;
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        pump(&mut activity, &f, &[], |_| {
            ["changed", "unchanged"]
                .into_iter()
                .all(|id| !f.calls(id, "discover_sessions").is_empty())
        })?;
        let original = f.calls("changed", "discover_sessions")[0]["pid"].clone();
        fs::write(f.dir.join("changed/options.json"), json!({"sessions":[{"harness":"changed","session_id":"replacement","cwd":"/workspace"}]}).to_string()).map_err(|e| e.to_string())?;
        drop(activity.reload(&f.config, &f.plugins, &HashSet::from(["changed".into()])));
        pump(&mut activity, &f, &[], |a| {
            !a.discovery_pending() && a.candidates().any(|row| row["session_id"] == "replacement")
        })?;
        assert!(!f
            .records("changed")
            .iter()
            .any(|r| r["pid"] == original && r["stage"] == "end"));
        assert_eq!(f.calls("unchanged", "discover_sessions").len(), 1);
        assert!(f.records("unchanged").iter().any(|r| r["stage"] == "end"));
        activity.shutdown()?;
        assert_reaped(&f);
        Ok(())
    }

    #[test]
    fn refresh_during_inflight_discovery_runs_the_new_generation_without_reap_delay(
    ) -> Result<(), String> {
        let mut f = Fixture::new()?;
        f.provider(
            "agent",
            json!({"discovery_delay":0.08,"sessions":[{"harness":"agent","session_id":"old","cwd":"/repo"}]}),
        )?;
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        pump(&mut activity, &f, &[], |_| {
            !f.calls("agent", "discover_sessions").is_empty()
        })?;
        fs::write(
            f.dir.join("agent/options.json"),
            json!({"sessions":[{"harness":"agent","session_id":"new","cwd":"/repo"}]}).to_string(),
        )
        .map_err(|e| e.to_string())?;
        activity.refresh_discovery();
        pump(&mut activity, &f, &[], |a| {
            a.candidates().any(|r| r["session_id"] == "new")
        })?;
        assert_eq!(f.calls("agent", "discover_sessions").len(), 2);
        assert!(!activity.candidates().any(|r| r["session_id"] == "old"));
        assert_eq!(activity.discovery_status()[0]["status"], "ready");
        activity.shutdown()?;
        Ok(())
    }
    #[test]
    fn subprocess_discovery_badge_and_setup_use_canonical_diagnostics() -> Result<(), String> {
        let mut f = Fixture::new()?;
        let rejection = json!({"ok":false,"error_code":"PROVIDER_OFFLINE","message":"The provider is unavailable","extra":{"private":"not displayed"}});
        f.provider("agent", json!({"response":rejection}))?;
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        pump(&mut activity, &f, &[], |a| {
            a.discovery_status()[0]["status"] == "error"
        })?;
        assert_eq!(
            activity.discovery_status()[0]["error"],
            "PROVIDER_OFFLINE: The provider is unavailable"
        );
        let request = badge_request(
            &f.config.state_dir,
            &activity.env,
            &Intent {
                session_id: "session".into(),
                endpoint_pid: None,
                badge: Some("1".into()),
            },
        );
        assert_eq!(
            send_badge(&f.plugins.plugins[0], &request, |_| Ok(())).unwrap_err(),
            "PROVIDER_OFFLINE: The provider is unavailable"
        );
        activity.remove_candidates("agent");
        assert!(activity.discovery_status()[0]["error"].is_null());
        fs::write(
            f.dir.join("agent/options.json"),
            json!({"response":{"sessions":[]}}).to_string(),
        )
        .map_err(|e| e.to_string())?;
        activity.refresh_discovery();
        pump(&mut activity, &f, &[], |a| {
            a.discovery_status()[0]["status"] == "error"
        })?;
        assert_eq!(
            activity.discovery_status()[0]["error"],
            "Session discovery failed",
            "A missing ok is not success"
        );
        assert_eq!(
            send_badge(&f.plugins.plugins[0], &request, |_| Ok(())).unwrap_err(),
            "Session badge update failed"
        );
        activity.shutdown()?;
        let mut plugin = f.plugins.plugins[0].clone();
        plugin.command = vec![
            "/usr/bin/python3".into(),
            "-c".into(),
            format!(
                "import json,sys; json.load(sys.stdin); print({:?})",
                rejection.to_string()
            ),
        ];
        assert_eq!(
            crate::setup::try_inspect_plugin_setup(&plugin).unwrap_err(),
            "PROVIDER_OFFLINE: The provider is unavailable"
        );
        assert_eq!(
            crate::setup::apply_plugin_setup(&plugin, false, false).unwrap_err(),
            "PROVIDER_OFFLINE: The provider is unavailable"
        );
        let status = crate::setup::inspect_setup_json(
            &f.config.state_dir,
            &f.config.config_path,
            &f.config.home_dir,
            &LoadedPlugins {
                plugins: vec![plugin],
                rejected_ids: HashSet::new(),
            },
        );
        assert_eq!(status["harnesses"][0]["status"], "error");
        assert!(status["harnesses"][0]["configured"].is_null());
        assert!(status["harnesses"][0]["error"]
            .as_str()
            .unwrap()
            .contains("provider is unavailable"));
        Ok(())
    }
    #[test]
    fn discovery_failure_revocation_and_recovery_use_provider_results() -> Result<(), String> {
        let mut f = Fixture::new()?;
        let candidate = json!({"harness":"agent","session_id":"kept","cwd":"/repo","title":"Work"});
        f.provider("agent", json!({"sessions":[candidate]}))?;
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        activity.discovery_demand(false, HashSet::from(["agent".into()]));
        let refresh = |a: &mut SessionActivity, options: Value| -> Result<(), String> {
            fs::write(f.dir.join("agent/options.json"), options.to_string())
                .map_err(|e| e.to_string())?;
            a.refresh_discovery();
            a.tick(&f.state, &[])?;
            assert!(a.discovery_pending());
            assert_eq!(a.candidates().count(), 0);
            Ok(())
        };
        for failure in [json!({"malformed":true}), json!({"response":{"ok":false}})] {
            let started = now_ms();
            refresh(&mut activity, json!({"sessions":[candidate]}))?;
            let launched = now_ms();
            pump(&mut activity, &f, &[], |a| !a.discovery_pending())?;
            let fresh = activity
                .candidates()
                .next()
                .ok_or("Missing confirmed candidate")?;
            assert!((started..=launched).contains(&fresh["discovered_at"].as_u64().unwrap()));
            assert_eq!(activity.available(&[], 1), vec![candidate.clone()]);
            refresh(&mut activity, failure)?;
            pump(&mut activity, &f, &[], |a| !a.discovery_pending())?;
            assert_eq!(activity.discovery_status()[0]["status"], "error");
            assert!(activity.available(&[], 1).is_empty());
            assert_eq!(activity.candidates().count(), 0);
        }
        let long = "é".repeat(501);
        let mut replacement = candidate.clone();
        replacement["title"] = json!(format!("  {long}  "));
        refresh(&mut activity, json!({"sessions":[replacement]}))?;
        pump(&mut activity, &f, &[], |a| !a.discovery_pending())?;
        replacement["title"] = json!("é".repeat(500));
        assert_eq!(activity.available(&[], 1), vec![replacement.clone()]);
        f.state.clone().invalidate_discovery("agent")?;
        assert_eq!(activity.candidates().count(), 0);
        refresh(&mut activity, json!({"sessions":[candidate]}))?;
        f.state.invalidate_discovery("agent")?;
        pump(&mut activity, &f, &[], |a| !a.discovery_pending())?;
        assert_eq!(activity.candidates().count(), 0);
        assert_eq!(activity.available(&[], 1), vec![replacement]);
        refresh(&mut activity, json!({"sessions":[]}))?;
        pump(&mut activity, &f, &[], |a| !a.discovery_pending())?;
        assert_eq!(activity.discovery_status()[0]["status"], "ready");
        assert!(activity.available(&[], 1).is_empty());
        Ok(())
    }

    #[test]
    fn discovery_worker_bound_and_forced_refresh() -> Result<(), String> {
        let mut f = Fixture::new()?;
        for i in 0..6 {
            f.provider(&format!("agent{i}"), json!({"discovery_delay":0.2}))?;
        }
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        activity.tick(&f.state, &[])?;
        assert_eq!(
            activity
                .discovery
                .iter()
                .filter(|p| p.flight.worker.is_some())
                .count(),
            MAX_WORKERS
        );
        assert!(activity.discovery[4..]
            .iter()
            .all(|p| p.flight.worker.is_none()));
        pump(&mut activity, &f, &[], |a| {
            a.discovery
                .iter()
                .all(|p| p.flight.worker.is_none() && p.due > Instant::now())
        })?;
        for i in 0..6 {
            assert_eq!(f.calls(&format!("agent{i}"), "discover_sessions").len(), 1);
        }
        activity.refresh_discovery();
        pump(&mut activity, &f, &[], |a| {
            a.discovery
                .iter()
                .all(|p| p.flight.worker.is_none() && p.due > Instant::now())
        })?;
        for i in 0..6 {
            assert_eq!(f.calls(&format!("agent{i}"), "discover_sessions").len(), 2);
        }
        activity.shutdown()?;
        assert_reaped(&f);
        Ok(())
    }

    #[test]
    fn discovery_filters_live_session_and_endpoint_per_hub_then_caps_eight() -> Result<(), String> {
        let mut f = Fixture::new()?;
        let mut sessions: Vec<_> = (0..14)
            .map(|i| {
                json!({"harness":"agent","session_id":format!("s{i}"),
            "cwd":"/workspace","endpoint_pid":100+i,"updated_at_ms":i,"listening_hubs":["forged"]})
            })
            .collect();
        sessions.push(sessions[13].clone());
        for invalid in [Value::Null, json!(4), json!(" \t")] {
            sessions.push(
                json!({"harness":"agent","session_id":"invalid","cwd":invalid,"updated_at_ms":999}),
            );
        }
        sessions.push(json!({"harness":"different","session_id":"forged"}));
        f.provider("agent", json!({"sessions":sessions}))?;
        f.provider("rejected", json!({"sessions":[]}))?;
        f.plugins.rejected_ids.insert("rejected".into());
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        pump(&mut activity, &f, &[], |a| !a.available(&[], 2).is_empty())?;
        let live = vec![
            json!({"harness":"agent","session_id":"s13","hub":"east"}),
            json!({"harness":"agent","session_id":"changed","endpoint_pid":113,"hub":"west"}),
            json!({"harness":"agent","session_id":"s12","hub":"east"}),
        ];
        let available = activity.available(&live, 2);
        assert_eq!(available.len(), 8);
        assert!(available
            .iter()
            .all(|row| row["title"].is_string() && row["cwd"] == "/workspace"));
        assert_eq!(available[0]["session_id"], "s12");
        assert_eq!(available[0]["listening_hubs"], json!(["east"]));
        assert!(available[1].get("listening_hubs").is_none());
        let available = activity.available(&live, 3);
        assert_eq!(available[0]["session_id"], "s13");
        assert_eq!(available[0]["listening_hubs"], json!(["east", "west"]));
        assert_eq!(
            available
                .iter()
                .filter(|r| r["session_id"] == "s13")
                .count(),
            1
        );
        assert!(activity
            .available(&[json!({"harness":"agent","session_id":"s13"})], 3)
            .iter()
            .all(|r| r["session_id"] != "s13"));
        assert!(f.calls("rejected", "discover_sessions").is_empty());
        activity.shutdown()?;
        Ok(())
    }

    #[test]
    fn badges_aggregate_all_live_hubs_include_passive_and_waking_and_preserve_zero(
    ) -> Result<(), String> {
        let mut f = Fixture::new()?;
        f.provider("agent", json!({}))?;
        let east = f.listener("east", "session", Some(42), 1)?;
        let west = f.listener("west", "session", Some(42), 1)?;
        f.listener("ghost", "session", Some(42), 2)?;
        f.seed("east", "e", "RECEIVED", Value::Null, now_ms() + 60_000)?;
        f.seed("east", "d", "DISPATCHING", Value::Null, now_ms() + 60_000)?;
        f.seed(
            "west",
            "w",
            "STORED_PASSIVE",
            Value::Null,
            now_ms() + 60_000,
        )?;
        f.seed("west", "sent", "DISPATCHED", Value::Null, now_ms() + 60_000)?;
        f.seed(
            "west",
            "acked",
            "RECEIVED",
            json!(now_ms()),
            now_ms() + 60_000,
        )?;
        f.seed("west", "expired", "STORED_PASSIVE", Value::Null, 1)?;
        f.seed(
            "west",
            "unsent",
            "EXPIRED_UNSENT",
            Value::Null,
            now_ms() + 60_000,
        )?;
        f.seed(
            "ghost",
            "g",
            "STORED_PASSIVE",
            Value::Null,
            now_ms() + 60_000,
        )?;
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        let live = vec![east.clone(), west.clone()];
        pump(&mut activity, &f, &live, |a| {
            a.badges
                .values()
                .all(|b| b.sent.as_ref() == Some(&b.desired))
        })?;
        assert_eq!(
            f.effect("agent").ok_or("Missing badge")?["badge"],
            "Cito · abcdefghijk (4)"
        );
        let count = f.calls("agent", "update_badge").len();
        for _ in 0..10 {
            activity.tick(&f.state, &live)?;
        }
        assert_eq!(
            f.calls("agent", "update_badge").len(),
            count,
            "Unchanged badge was sent twice"
        );
        f.state.db.execute(
            "UPDATE local_listeners SET status='UNLISTENED' WHERE hub='east'",
            &[],
        )?;
        pump(&mut activity, &f, std::slice::from_ref(&west), |a| {
            a.badges
                .values()
                .all(|b| b.sent.as_ref() == Some(&b.desired))
        })?;
        assert_eq!(
            f.effect("agent").ok_or("Missing badge")?["badge"],
            "Cito · abcdefghijk (2)"
        );
        f.state.db.execute(
            "UPDATE local_signals SET acknowledged_at_ms=? WHERE hub='west'",
            &[json!(now_ms())],
        )?;
        pump(&mut activity, &f, std::slice::from_ref(&west), |a| {
            a.badges
                .values()
                .all(|b| b.sent.as_ref() == Some(&b.desired))
        })?;
        assert_eq!(
            f.effect("agent").ok_or("Missing badge")?["badge"],
            "Cito · abcdefghijk"
        );
        pump(&mut activity, &f, &[], |a| {
            a.badges
                .values()
                .all(|b| b.sent.as_ref() == Some(&b.desired))
        })?;
        assert!(f.effect("agent").ok_or("Missing badge")?["badge"].is_null());
        let count = f.calls("agent", "update_badge").len();
        activity.shutdown()?;
        assert_eq!(
            f.calls("agent", "update_badge").len(),
            count,
            "Shutdown repeated known null"
        );
        assert_reaped(&f);
        Ok(())
    }

    #[test]
    fn badge_failure_retries_and_session_switch_serializes_latest_intent() -> Result<(), String> {
        let mut f = Fixture::new()?;
        f.provider("agent", json!({"badge_delay":0.15,"fail_once":true}))?;
        let old = f.listener("east", "old", Some(42), 1)?;
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        activity.tick(&f.state, std::slice::from_ref(&old))?;
        // A second DB handle remains usable while the actual provider sleeps.
        let other = LocalState::open(&f.config.state_dir)?;
        assert_eq!(other.active()?.len(), 1);
        pump(&mut activity, &f, std::slice::from_ref(&old), |_| {
            !f.calls("agent", "update_badge").is_empty()
        })?;
        f.state.db.execute("UPDATE local_listeners SET session_id='new',binding_generation=2,public_id='new-session-abcdefghijkl' WHERE hub='east'",&[])?;
        let new = f
            .state
            .listener("east", "c_abcdefghijkl")?
            .ok_or("Missing listener")?;
        for _ in 0..5 {
            activity.tick(&f.state, std::slice::from_ref(&new))?;
        }
        pump(&mut activity, &f, std::slice::from_ref(&new), |a| {
            a.badges
                .values()
                .all(|b| b.sent.as_ref() == Some(&b.desired))
        })?;
        let calls = f.calls("agent", "update_badge");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["req"]["session_id"], "old");
        assert_eq!(calls[1]["req"]["session_id"], "new");
        assert_eq!(
            f.effect("agent").ok_or("Missing badge")?["badge"],
            "Cito · new-session"
        );
        assert!(!f.records("agent").iter().any(|r| r["stage"] == "overlap"));
        let first_end = f
            .records("agent")
            .into_iter()
            .find(|r| r["stage"] == "end" && r["req"]["op"] == "update_badge")
            .ok_or("Missing completion")?;
        assert!(first_end["at"].as_f64() <= calls[1]["at"].as_f64());
        activity.shutdown()?;
        assert!(f.effect("agent").ok_or("Missing clear")?["badge"].is_null());
        assert_reaped(&f);
        Ok(())
    }

    #[test]
    fn stale_success_cannot_replace_newer_desired_and_shutdown_reaps_inflight() -> Result<(), String>
    {
        let mut f = Fixture::new()?;
        f.provider(
            "agent",
            json!({"badge_delay":0.2,"discovery_delay":30,"descendant":true}),
        )?;
        let row = f.listener("east", "session", Some(42), 1)?;
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        pump(&mut activity, &f, std::slice::from_ref(&row), |_| {
            !f.calls("agent", "update_badge").is_empty()
                && f.records("agent").iter().any(|r| r["stage"] == "child")
        })?;
        f.seed(
            "east",
            "new",
            "STORED_PASSIVE",
            Value::Null,
            now_ms() + 60_000,
        )?;
        pump(&mut activity, &f, std::slice::from_ref(&row), |a| {
            a.badges
                .values()
                .all(|b| b.sent.as_ref() == Some(&b.desired))
        })?;
        let calls = f.calls("agent", "update_badge");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["req"]["badge"], "Cito · abcdefghijk");
        assert_eq!(calls[1]["req"]["badge"], "Cito · abcdefghijk (1)");
        let started = Instant::now();
        activity.shutdown()?;
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(activity.discovery.iter().all(|p| p.flight.worker.is_none()));
        assert!(activity.badges.values().all(|p| p.worker.is_none()));
        assert_reaped(&f);
        Ok(())
    }

    #[test]
    fn only_live_agent_badges_read_signals() -> Result<(), String> {
        let f = Fixture::new()?;
        let agent = f.listener("work", "session", Some(42), 1)?;
        f.seed("work", "one", "RECEIVED", Value::Null, now_ms() + 60_000)?;
        f.seed(
            "work",
            "two",
            "STORED_PASSIVE",
            Value::Null,
            now_ms() + 60_000,
        )?;
        f.listener("personal", "default", None, 1)?;
        f.state.db.execute(
            "UPDATE local_listeners SET harness='human' WHERE hub='personal'",
            &[],
        )?;
        let human = f
            .state
            .listener("personal", "c_abcdefghijkl")?
            .ok_or("Missing human")?;
        let fenced = f.listener("old", "previous", None, 1)?;
        f.state.fence_listener_for(&fenced, "disconnected")?;
        let live = [agent.clone(), human];
        let desired = desired_badges(&f.state, &live)?;
        assert_eq!(
            desired[&Target::of(&agent)].badge.as_deref(),
            Some("Cito · abcdefghijk (2)")
        );

        f.state.pause_listener_for(&agent)?;
        // Any unread lookup now fails, even one whose result would be discarded.
        f.state
            .db
            .execute("ALTER TABLE local_signals RENAME TO hidden_signals", &[])?;
        let desired = desired_badges(&f.state, &live)?;
        assert_eq!(desired[&Target::of(&agent)].badge, None);
        assert_eq!(desired[&Target::of(&fenced)].badge, None);
        assert!(!desired.contains_key(&Target::of(&live[1])));
        Ok(())
    }

    #[test]
    fn badge_restore_fences_stale_owner_and_uses_separate_endpoints() -> Result<(), String> {
        let mut f = Fixture::new()?;
        f.provider("agent", json!({}))?;
        let row = f.listener("east", "session", None, 1)?;
        let second = f.listener("west", "session", Some(44), 1)?;
        f.seed(
            "east",
            "e",
            "STORED_PASSIVE",
            Value::Null,
            now_ms() + 60_000,
        )?;
        f.seed(
            "west",
            "w",
            "STORED_PASSIVE",
            Value::Null,
            now_ms() + 60_000,
        )?;
        let mut stale = row.clone();
        stale["owner_token"] = json!("previous");
        let desired = desired_badges(&f.state, &[stale, second.clone()])?;
        assert_eq!(desired[&Target::of(&row)].badge, None);
        assert_eq!(
            desired[&Target::of(&second)].badge,
            Some("Cito · abcdefghijk (1)".into())
        );
        let desired = desired_badges(&f.state, &[row.clone(), second])?;
        assert_eq!(
            desired[&Target::of(&row)].badge,
            Some("Cito · abcdefghijk (1)".into())
        );
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        // Restoring one session counts its persisted unread even without a new
        // admission; persisted ACTIVE rows for other endpoints remain inactive.
        pump(&mut activity, &f, std::slice::from_ref(&row), |a| {
            a.badges
                .values()
                .all(|b| b.sent.as_ref() == Some(&b.desired))
        })?;
        assert_eq!(
            activity.badges.len(),
            1,
            "The acknowledged inactive endpoint must retire"
        );
        assert!(f
            .calls("agent", "update_badge")
            .iter()
            .any(|r| r["req"]["endpoint_pid"] == 44 && r["req"]["badge"].is_null()));
        assert!(activity.badges[&Target::of(&row)]
            .sent
            .as_ref()
            .is_some_and(|i| i.badge.is_some()));
        activity.shutdown()?;
        Ok(())
    }

    #[test]
    fn failed_badge_retries_unchanged_desired_after_possible_effect() -> Result<(), String> {
        let mut f = Fixture::new()?;
        f.provider("agent", json!({"fail_once":true}))?;
        let row = f.listener("east", "session", Some(42), 1)?;
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        pump(&mut activity, &f, std::slice::from_ref(&row), |a| {
            a.badges
                .values()
                .all(|b| b.sent.as_ref() == Some(&b.desired))
        })?;
        let calls = f.calls("agent", "update_badge");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["req"], calls[1]["req"]);
        assert!(
            calls[1]["at"].as_f64().ok_or("Missing time")?
                - calls[0]["at"].as_f64().ok_or("Missing time")?
                >= BADGE_RETRY.as_secs_f64()
        );
        assert_eq!(
            f.effect("agent").ok_or("Missing effect")?["badge"],
            "Cito · abcdefghijk"
        );
        activity.shutdown()?;
        assert_reaped(&f);
        Ok(())
    }

    #[test]
    fn shutdown_cancels_badge_worker_and_reaps_failed_final_clear() -> Result<(), String> {
        let mut f = Fixture::new()?;
        f.provider(
            "agent",
            json!({"badge_delay":30,"discovery_delay":30,"descendant":true}),
        )?;
        let row = f.listener("east", "session", Some(42), 1)?;
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        pump(&mut activity, &f, std::slice::from_ref(&row), |_| {
            !f.calls("agent", "update_badge").is_empty()
                && f.records("agent").iter().any(|r| r["stage"] == "child")
        })?;
        let started = Instant::now();
        assert!(
            activity.shutdown().is_err(),
            "A hanging final clear must report failure"
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(activity.badges.values().all(|b| b.worker.is_none()));
        assert!(activity.discovery.iter().all(|p| p.flight.worker.is_none()));
        assert!(!f.records("agent").iter().any(|r| r["stage"] == "overlap"));
        assert_reaped(&f);
        let calls = f.calls("agent", "update_badge").len();
        activity.shutdown()?;
        assert_eq!(f.calls("agent", "update_badge").len(), calls);
        Ok(())
    }

    #[test]
    fn badge_backoff_is_exponential_capped_and_inactive_failures_retire() -> Result<(), String> {
        assert_eq!(
            (1..=9)
                .map(|n| badge_retry(n).as_millis())
                .collect::<Vec<_>>(),
            vec![250, 500, 1000, 2000, 4000, 8000, 15000, 15000, 15000]
        );
        let mut f = Fixture::new()?;
        f.provider("agent", json!({}))?;
        let row = f.listener("east", "session", Some(42), 1)?;
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        pump(&mut activity, &f, std::slice::from_ref(&row), |a| {
            a.badges
                .values()
                .all(|b| b.sent.as_ref() == Some(&b.desired))
        })?;
        fs::write(
            f.dir.join("agent/options.json"),
            json!({"response":{"ok":false}}).to_string(),
        )
        .map_err(|e| e.to_string())?;
        let mut observation = row.clone();
        observation["badge"] = json!("outdated badge");
        for failures in 1..=3 {
            activity.observe_badge(&observation);
            pump(&mut activity, &f, std::slice::from_ref(&row), |a| {
                a.badges[&Target::of(&row)].failures == failures
            })?;
            assert!(
                activity.badges[&Target::of(&row)].retry_at
                    > Instant::now() + badge_retry(failures) - Duration::from_millis(50)
            );
        }
        f.seed(
            "east",
            "new",
            "STORED_PASSIVE",
            Value::Null,
            now_ms() + 60_000,
        )?;
        activity.tick(&f.state, std::slice::from_ref(&row))?;
        assert_eq!(
            activity.badges[&Target::of(&row)].failures,
            0,
            "New intent resets retry history"
        );
        pump(&mut activity, &f, &[], |a| {
            a.badges[&Target::of(&row)].failures == 1
        })?;
        for failures in 2..=INACTIVE_FAILURE_LIMIT {
            activity.observe_badge(&observation);
            pump(&mut activity, &f, &[], |a| {
                match a.badges.get(&Target::of(&row)) {
                    Some(b) => failures < INACTIVE_FAILURE_LIMIT && b.failures == failures,
                    None => failures == INACTIVE_FAILURE_LIMIT,
                }
            })?;
        }
        assert_eq!(
            f.calls("agent", "update_badge")
                .iter()
                .filter(|call| call["req"]["badge"].is_null())
                .count(),
            INACTIVE_FAILURE_LIMIT as usize
        );
        let count = f.calls("agent", "update_badge").len();
        for _ in 0..3 {
            activity.tick(&f.state, &[])?;
        }
        assert!(activity.badges.is_empty());
        assert_eq!(f.calls("agent", "update_badge").len(), count);
        activity.shutdown()?;
        assert_reaped(&f);
        Ok(())
    }

    #[test]
    fn badge_formatter_preserves_public_slug_fallback_truncation_and_zero() {
        for public in ["topic-name-abcdefghijkl", "topic-name-0123456789abcdef"] {
            assert_eq!(
                format_badge(&json!({"public_id":public,"harness":"agent"}), 0),
                "Cito · topic-name"
            );
        }
        assert_eq!(
            format_badge(
                &json!({"public_id":"------------x-abcdefghijkl","harness":"agent"}),
                5
            ),
            "Cito · agent (5)"
        );
        assert_eq!(
            format_badge(
                &json!({"tag":"___","repo":"acme/some-repo","cwd":"/work/other","harness":"agent"}),
                0
            ),
            "Cito · some-repo"
        );
        assert_eq!(
            format_badge(&json!({"tag":"abcdefghijkl-long","harness":"agent"}), 2),
            "Cito · abcdefghijkl (2)"
        );
    }
}
