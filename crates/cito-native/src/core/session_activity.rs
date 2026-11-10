//! Cached discovery and serialized, endpoint-local badge effects. No worker owns a DB lock.
use super::{identity::string, state::LocalState};
use crate::plugins::{
    now_ms, plugin_request_with_progress, LoadedPlugins, NativeConfig, PluginDescriptor,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const DISCOVERY_TIMEOUT: Duration = Duration::from_millis(1500);
const REFRESH_INTERVAL: Duration = Duration::from_secs(5);
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
    due: Instant,
    worker: Option<Worker<Vec<Value>>>,
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
    env: Value,
    discovery: Vec<Discovery>,
    badges: BTreeMap<Target, Badge>,
    history_loaded: bool,
    stopping: Arc<AtomicBool>,
    closed: bool,
}
impl SessionActivity {
    pub fn new(config: &NativeConfig, plugins: &LoadedPlugins) -> Self {
        Self {
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
                    due: Instant::now(),
                    worker: None,
                })
                .collect(),
            badges: BTreeMap::new(),
            history_loaded: false,
            stopping: Arc::new(AtomicBool::new(false)),
            closed: false,
        }
    }

    /// Mark discovery due without spawning, waiting, or duplicating in-flight work.
    pub fn refresh_discovery(&mut self) {
        for provider in &mut self.discovery {
            provider.due = Instant::now();
        }
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
        let changed = self.reap_discovery();
        self.reap_badges();
        // One short transaction captures both rows and counts. Persisted ACTIVE
        // rows alone never establish liveness and never add unread to a badge.
        let mut desired = desired_badges(state, live_active)?;
        // Historical endpoints need one bounded clear after startup, not a fresh
        // retry lifecycle on every DB scan once their badge entry has retired.
        if self.history_loaded {
            desired.retain(|target, intent| {
                intent.badge.is_some() || self.badges.contains_key(target)
            });
        }
        self.history_loaded = true;
        for (target, badge) in &mut self.badges {
            if !desired.contains_key(target) && badge.desired.badge.is_some() {
                badge.desired.badge = None;
                badge.failures = 0;
                badge.retry_at = Instant::now();
            }
        }
        for (target, intent) in desired {
            if let Some(badge) = self.badges.get_mut(&target) {
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
        self.badges.retain(|_, badge| {
            badge.worker.is_some()
                || badge.desired.badge.is_some()
                || (badge.sent.as_ref() != Some(&badge.desired)
                    && badge.failures < INACTIVE_FAILURE_LIMIT)
        });
        self.start_badges()?;
        let mut capacity = MAX_WORKERS
            .saturating_sub(self.discovery.iter().filter(|p| p.worker.is_some()).count());
        for provider in &mut self.discovery {
            if capacity == 0 {
                break;
            }
            if provider.worker.is_some() || provider.due > Instant::now() {
                continue;
            }
            let plugin = provider.plugin.clone();
            let request =
                json!({"op":"discover_sessions","state_dir":self.state_dir,"env":self.env});
            let stopping = self.stopping.clone();
            provider.worker = Some(
                thread::Builder::new()
                    .name("cito-discovery".into())
                    .spawn(move || {
                        let response = plugin_request_with_progress(
                            &plugin,
                            &request,
                            DISCOVERY_TIMEOUT,
                            |_| check_running(&stopping),
                        )?;
                        if response["ok"] == false {
                            return Err(format!("Discovery rejected: {response}"));
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
                            })
                            .cloned()
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
            if !provider.worker.as_ref().is_some_and(|w| w.is_finished()) {
                continue;
            }
            if let Some(worker) = provider.worker.take() {
                let rows = match worker.join() {
                    Ok(Ok(rows)) => rows,
                    Ok(Err(error)) => {
                        eprintln!("cito discovery {}: {error}", provider.plugin.id);
                        Vec::new()
                    }
                    Err(_) => {
                        eprintln!("cito discovery {}: worker panicked", provider.plugin.id);
                        Vec::new()
                    }
                };
                changed |= rows != provider.cached;
                provider.cached = rows;
                provider.due = Instant::now() + REFRESH_INTERVAL;
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
                match worker.join() {
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
            badge.worker = Some(
                thread::Builder::new()
                    .name("cito-badge".into())
                    .spawn(move || {
                        send_badge(&plugin, &request, &stopping)?;
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
            if let Some(worker) = provider.worker.take() {
                let _ = worker.join();
            }
        }
        for badge in self.badges.values_mut() {
            if let Some(worker) = badge.worker.take() {
                badge.sent = worker.join().ok().and_then(Result::ok);
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
                        .spawn_scoped(scope, move || {
                            send_badge(plugin, request, &AtomicBool::new(false))
                        }) {
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
    stopping: &AtomicBool,
) -> Result<(), String> {
    let response =
        plugin_request_with_progress(plugin, request, BADGE_TIMEOUT, |_| check_running(stopping))?;
    if response["ok"] == true {
        Ok(())
    } else {
        Err(format!("Badge rejected: {response}"))
    }
}

fn desired_badges(
    state: &LocalState,
    live_active: &[Value],
) -> Result<BTreeMap<Target, Intent>, String> {
    let rows = state.db.transaction(|db| db.query("SELECT l.*, (SELECT COUNT(*) FROM local_signals s WHERE s.hub=l.hub AND s.listener_id=l.listener_id AND s.acknowledged_at_ms IS NULL AND s.expires_at_ms>? AND s.state!='EXPIRED_UNSENT') AS badge_unread FROM local_listeners l ORDER BY (l.status='ACTIVE') DESC,l.binding_generation DESC,l.hub ASC,l.listener_id ASC", &[json!(now_ms())]))?;
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
        let target = rows
            .iter()
            .copied()
            .find(|row| is_live(row))
            .unwrap_or(rows[0]);
        let active: Vec<_> = rows
            .iter()
            .copied()
            .filter(|row| is_live(row) && row["session_id"] == target["session_id"])
            .collect();
        let badge = if active.is_empty() {
            None
        } else {
            let unread: u64 = active
                .iter()
                .map(|r| r["badge_unread"].as_u64().unwrap_or(0))
                .sum();
            Some(format_badge(target, unread))
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
        format!("🔔 {short} ({unread})")
    } else {
        format!("🔔 {short}")
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
            self.plugins.plugins.push(PluginDescriptor {
                id: id.into(),
                kind: "agent".into(),
                display_name: id.into(),
                tier: "custom".into(),
                sf_symbol: "bolt".into(),
                tint: "blue".into(),
                cli_flag: false,
                plugin_dir: path.clone(),
                is_builtin: false,
                channel_env: None,
                fields: Vec::new(),
                post_setup_hint: None,
                post_disconnect_hint: None,
                command: vec![
                    "/usr/bin/env".into(),
                    format!("HOME={}", self.config.home_dir.display()),
                    format!("CITO_STATE_DIR={}", self.config.state_dir.display()),
                    format!("CITO_CONFIG_PATH={}", self.config.config_path.display()),
                    "/usr/bin/python3".into(),
                    path.join("provider.py").to_string_lossy().into(),
                ],
            });
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
            self.state.db.execute("INSERT INTO local_listeners(hub,listener_id,public_id,principal_id,owner_user,host_id,host_name,harness,session_id,endpoint_pid,cwd,title,about,scope,status,binding_generation,owner_token) VALUES(?,'c_abcdefghijkl','abcdefghijk--more-abcdefghijkl','none#alice','alice','host','host','agent',?,?,'/workspace','Target','','workspace','ACTIVE',?,'owner')",&[json!(hub),json!(session),json!(pid),json!(generation)])?;
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
            self.state.db.execute("INSERT INTO local_signals(hub,signal_id,listener_id,seq,binding_generation,from_user,from_host_name,from_title,message,created_at,expires_at_ms,state,acknowledged_at_ms) VALUES(?,?,'c_abcdefghijkl',1,1,'bob','remote','Sender','hello','2026-01-01T00:00:00.000Z',?,?,?)",&[json!(hub),json!(id),json!(expires),json!(state),acked])?;
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
    fn discovery_concurrent_deadline_failure_isolation_and_cache_only_read() -> Result<(), String> {
        let mut f = Fixture::new()?;
        f.provider(
            "fast",
            json!({"discovery_delay":0.2,"sessions":[{"harness":"fast","session_id":"one"}]}),
        )?;
        f.provider("slow", json!({"discovery_delay":30,"descendant":true}))?;
        f.provider("invalid", json!({"malformed":true}))?;
        f.provider(
            "other",
            json!({"discovery_delay":0.2,"sessions":[{"harness":"other","session_id":"two"}]}),
        )?;
        let mut activity = SessionActivity::new(&f.config, &f.plugins);
        let started = Instant::now();
        activity.tick(&f.state, &[])?;
        assert!(started.elapsed() < Duration::from_millis(200));
        pump(&mut activity, &f, &[], |a| a.available(&[], 1).len() == 2)?;
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
            a.discovery.iter().all(|p| p.worker.is_none())
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
                .filter(|p| p.worker.is_some())
                .count(),
            MAX_WORKERS
        );
        assert!(activity.discovery[4..].iter().all(|p| p.worker.is_none()));
        pump(&mut activity, &f, &[], |a| {
            a.discovery
                .iter()
                .all(|p| p.worker.is_none() && p.due > Instant::now())
        })?;
        for i in 0..6 {
            assert_eq!(f.calls(&format!("agent{i}"), "discover_sessions").len(), 1);
        }
        activity.refresh_discovery();
        pump(&mut activity, &f, &[], |a| {
            a.discovery
                .iter()
                .all(|p| p.worker.is_none() && p.due > Instant::now())
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
            "endpoint_pid":100+i,"updated_at_ms":i,"listening_hubs":["forged"]})
            })
            .collect();
        sessions.push(sessions[13].clone());
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
            "🔔 abcdefghijk (4)"
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
            "🔔 abcdefghijk (2)"
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
            "🔔 abcdefghijk"
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
            "🔔 new-session"
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
        assert_eq!(calls[0]["req"]["badge"], "🔔 abcdefghijk");
        assert_eq!(calls[1]["req"]["badge"], "🔔 abcdefghijk (1)");
        let started = Instant::now();
        activity.shutdown()?;
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(activity.discovery.iter().all(|p| p.worker.is_none()));
        assert!(activity.badges.values().all(|p| p.worker.is_none()));
        assert_reaped(&f);
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
            Some("🔔 abcdefghijk (1)".into())
        );
        let desired = desired_badges(&f.state, &[row.clone(), second])?;
        assert_eq!(
            desired[&Target::of(&row)].badge,
            Some("🔔 abcdefghijk (1)".into())
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
            "🔔 abcdefghijk"
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
        assert!(activity.discovery.iter().all(|p| p.worker.is_none()));
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
        // Controlled worker completions exercise the real reap/tick policy without
        // sleeping through the capped 15-second retry interval.
        for failures in 1..=3 {
            let badge = activity
                .badges
                .get_mut(&Target::of(&row))
                .ok_or("Missing badge")?;
            badge.attempted = Some(badge.desired.clone());
            badge.worker = Some(thread::spawn(|| Err("unavailable".into())));
            while !badge.worker.as_ref().is_some_and(|w| w.is_finished()) {
                thread::yield_now();
            }
            activity.reap_badges();
            let badge = &activity.badges[&Target::of(&row)];
            assert_eq!(badge.failures, failures);
            assert!(
                badge.retry_at > Instant::now() + badge_retry(failures) - Duration::from_millis(50)
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
        pump(&mut activity, &f, std::slice::from_ref(&row), |a| {
            a.badges.values().all(|b| b.worker.is_none())
        })?;
        let badge = activity
            .badges
            .get_mut(&Target::of(&row))
            .ok_or("Missing badge")?;
        badge.desired.badge = None;
        badge.sent = None;
        badge.failures = INACTIVE_FAILURE_LIMIT;
        activity.tick(&f.state, &[])?;
        assert!(activity.badges.is_empty());
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
                "🔔 topic-name"
            );
        }
        assert_eq!(
            format_badge(
                &json!({"public_id":"------------x-abcdefghijkl","harness":"agent"}),
                5
            ),
            "🔔 agent (5)"
        );
        assert_eq!(
            format_badge(
                &json!({"tag":"___","repo":"acme/some-repo","cwd":"/work/other","harness":"agent"}),
                0
            ),
            "🔔 some-repo"
        );
        assert_eq!(
            format_badge(&json!({"tag":"abcdefghijkl-long","harness":"agent"}), 2),
            "🔔 abcdefghijkl (2)"
        );
    }
}
