import { randomUUID } from "node:crypto";
import { z } from "zod";
import { Events, ErrorCode } from "nats";
import { UiBridge, type UiOptions, type UiToDaemonAction } from "../ui/index.js";
import { setTimeout as delay } from "node:timers/promises";
import { getAdapter, resolveCaller, type Caller, type CachedSessionQuery } from "../adapters/index.js";
import { KeyedMutex, listenMutex, CitoError, brokerNow, evaluateLease, RETENTION_MS, isJsApiError, connectNats, provisionNats, loadConfig, getHostId,
  localPrincipal, listenerId, listenerSlug, normalizeSlug, publicListenerId, toCanonicalListenerId,
  senderEndpointKey, parseInput, successResult, errorResult, parseScopedId, formatScopedId, sha256,
  listenInputSchema, unlistenInputSchema, findInputSchema, sendInputSchema, inboxInputSchema,
  type CitoConfig, type Principal } from "../shared/index.js";
import { LocalState, registration, registrationSchema, type ListenerRow, type Registration, type SentRow, type SignalRow } from "./state.js";
import { OidcLogin, type BrowserOpener } from "./oidc.js";
import { Receiver, encode, refreshSessionBadge, type Broker, type ReceiverOptions } from "./receiver.js";

export interface ServiceOptions extends ReceiverOptions { browserOpener?: BrowserOpener; ui?: UiOptions; onQuit?: () => void | Promise<void> }
const ackSchema = z.object({ signal_id: z.string(), status: z.enum(["wake_accepted", "stored_passive"]), held_reason: z.string().nullable() });
const displayId = (row: ListenerRow | Registration): string => row.public_id || publicListenerId(listenerSlug(row), row.listener_id);
const warning = (hub: string, error: unknown) => ({ hub, code: error instanceof CitoError ? error.code : "NATS_UNAVAILABLE", message: error instanceof Error ? error.message : String(error) });

/** A slot owns authentication and transport. No connection/token can leak across hubs. */
export class HubSlot {
  private brokerPromise?: Promise<Broker>;
  private broker?: Broker;
  private rejectedHash?: string;
  principal?: Principal;
  manuallyDisconnected = false;
  status: "connected" | "connecting" | "auth_required" | "disconnected" = "disconnected";
  error: string | null = null;
  private connectionAbort = new AbortController();
  private restoreRetry?: AbortController;
  private readonly actions = new KeyedMutex();
  readonly receivers = new Map<string, Receiver>();
  private readonly login?: OidcLogin;
  private readonly background = new Set<Promise<unknown>>();
  private closing = false;
  constructor(readonly name: string, readonly config: CitoConfig, private readonly options: ServiceOptions,
    private readonly restore: (broker: Broker) => Promise<void>, private readonly cancel: () => void, private readonly changed: () => void = () => {}) {
    if (config.authMode === "oidc") this.login = new OidcLogin(config, options.browserOpener, name);
    else this.principal = localPrincipal(config.user);
  }
  async identity(offline = false): Promise<Principal> {
    if (offline && this.login) return this.login.offlinePrincipal();
    if (!this.principal) await this.network();
    return this.principal!;
  }
  async network({ interactive = true }: { interactive?: boolean } = {}): Promise<Broker> {
    if (this.manuallyDisconnected) throw new CitoError("NATS_UNAVAILABLE", `Hub '${this.name}' is manually disconnected`);
    if (this.closing) throw new CitoError("NATS_UNAVAILABLE", "Service is closing");
    if (this.broker && !this.broker.nc.isClosed() && !this.rejectedHash) { await this.restore(this.broker); return this.broker; }
    return this.brokerPromise ??= (async () => {
      const signal = this.connectionAbort.signal;
      this.status = "connecting"; this.error = null; this.changed();
      if (this.rejectedHash) await this.broker?.nc.close();
      let token = this.login ? (this.rejectedHash ? await this.login.recover(this.rejectedHash, interactive, signal) : await this.login.current(interactive, signal)) : undefined;
      this.rejectedHash = undefined;
      const connect = () => connectNats({ servers: this.config.natsUrl, token: token?.id_token,
        name: "cito-mcp-" + this.name, timeout: 10_000, maxReconnectAttempts: -1 });
      let nc;
      try { nc = await connect(); }
      catch (cause) {
        if (this.login && /authorization|authentication/i.test(String(cause))) {
          token = await this.login.recover(token?.token_sha256 ?? null, interactive, signal);
          try { nc = await connect(); } catch (cause) { throw new CitoError("UNAUTHORIZED", "NATS rejected refreshed OIDC token", { cause }); }
        } else throw new CitoError("NATS_UNAVAILABLE", "Cannot connect to NATS", { cause });
      }
      try {
        signal.throwIfAborted();
        if (this.closing || this.manuallyDisconnected) throw new CitoError("NATS_UNAVAILABLE", "Hub disconnected");
        const broker = { ...await provisionNats(nc), nc };
        signal.throwIfAborted();
        this.broker = broker; this.status = "connected";
        if (token) this.principal = { principal_id: token.verified_principal_id, owner_user: token.verified_user };
        this.track(this.monitor(this.broker, token?.token_sha256));
        await this.restore(this.broker);
        return this.broker;
      } catch (cause) { await nc.close(); throw new CitoError("NATS_UNAVAILABLE", "NATS topology unavailable", { cause }); }
    })().catch(error => {
      this.error = String(error);
      this.status = this.manuallyDisconnected || this.closing ? "disconnected" : error instanceof CitoError && ["UNAUTHORIZED", "OIDC_LOGIN_TIMEOUT"].includes(error.code) ? "auth_required" : "disconnected";
      throw error;
    }).finally(() => { this.brokerPromise = undefined; this.changed(); });
  }
  private track(task: Promise<void>): void {
    this.background.add(task);
    void task.catch(error => {
      if (!this.closing && !(error instanceof Error && error.name === "AbortError")) (this.options.diagnostic ?? console.error)(String(error));
    }).finally(() => this.background.delete(task));
  }
  private retryRestoration(broker: Broker): void {
    this.restoreRetry?.abort();
    const retry = this.restoreRetry = new AbortController();
    const signal = AbortSignal.any([retry.signal, this.connectionAbort.signal]);
    const active = () => !signal.aborted && !this.closing && !this.manuallyDisconnected
      && this.broker === broker && !broker.nc.isClosed() && this.status === "connected";
    this.track((async () => {
      for (const pause of [250, 1000, 3000]) {
        if (!active()) return;
        await delay(pause, undefined, { signal });
        if (!active()) return;
        try {
          await this.restore(broker);
          if (active()) { this.error = null; this.changed(); }
          return;
        } catch (error) {
          if (!active()) return;
          this.error = String(error); this.changed();
        }
      }
    })());
  }
  private async monitor(broker: Broker, tokenHash?: string): Promise<void> {
    const statuses = broker.nc.status();
    if (!("stop" in statuses) || typeof statuses.stop !== "function") throw new Error("Unsupported NATS status iterator");
    const stop = statuses.stop.bind(statuses);
    const current = () => !this.closing && !this.manuallyDisconnected && this.broker === broker;
    const authError = (code: unknown) => [ErrorCode.AuthorizationViolation, ErrorCode.AuthenticationExpired, ErrorCode.PermissionsViolation].includes(code as ErrorCode);
    let terminalAuth: Error | undefined;
    void broker.nc.closed().then(error => {
      if (current()) {
        const auth = terminalAuth || (error && "code" in error && authError(error.code));
        if (auth && tokenHash) this.rejectedHash = tokenHash;
        this.restoreRetry?.abort(); this.cancel();
        this.status = auth ? "auth_required" : "disconnected";
        this.error = String(terminalAuth ?? error ?? "NATS connection closed unexpectedly");
        this.changed();
      }
      stop();
    });
    for await (const status of statuses) {
      if (!current()) break;
      if (status.type === Events.Disconnect) {
        this.restoreRetry?.abort(); this.status = "connecting"; this.cancel(); this.changed();
      }
      if (status.type === Events.Error && authError(status.data)) {
        terminalAuth = new Error(String(status.data));
        if (tokenHash) this.rejectedHash = tokenHash;
        this.restoreRetry?.abort(); this.cancel();
        this.status = "auth_required"; this.error = terminalAuth.message; this.changed();
        await broker.nc.close(); break;
      }
      if (status.type === Events.Reconnect) {
        this.status = "connected"; this.error = null; this.changed();
        try { await this.restore(broker); }
        catch (error) {
          if (current() && !broker.nc.isClosed() && this.status === "connected") {
            this.error = String(error); this.changed(); this.retryRestoration(broker);
          }
        }
      }
    }
  }
  connectedBroker(): Broker | undefined { return this.broker && !this.broker.nc.isClosed() && !this.manuallyDisconnected ? this.broker : undefined; }
  async disconnect(): Promise<void> {
    this.manuallyDisconnected = true; this.connectionAbort.abort(); this.cancel();
    await this.actions.run(this.name, async () => {
      await this.broker?.nc.close(); await this.brokerPromise?.catch(() => {}); await this.broker?.nc.close();
      this.broker = undefined; this.status = "disconnected"; this.changed();
    });
  }
  async reconnect(interactive: boolean): Promise<void> {
    await this.actions.run(this.name, async () => {
      await this.brokerPromise?.catch(() => {});
      if (this.closing) throw new CitoError("NATS_UNAVAILABLE", "Service is closing");
      this.manuallyDisconnected = false; this.connectionAbort = new AbortController();
      await this.network({ interactive });
    });
  }
  async close(): Promise<void> {
    this.closing = true; this.connectionAbort.abort(); await this.broker?.nc.close();
    await this.brokerPromise?.catch(() => {}); await this.broker?.nc.close(); await Promise.allSettled([...this.background]);
  }
}

/** One shared SQLite database, lazily connected independent HubSlots. */
export class CitoService {
  readonly receivers = new Map<string, Receiver>();
  ui?: UiBridge;
  private uiStarting?: Promise<void>;
  readonly slots = new Map<string, HubSlot>();
  private closing = false;
  private readonly shuttingDown = new AbortController();
  private closePromise?: Promise<void>;
  private readonly calls = new Set<Promise<unknown>>();
  private readonly retirements = new Set<Promise<void>>();
  private retire(receiver?: Receiver): void {
    if (!receiver) return;
    const task = receiver.stop().catch(error => this.log(error)); this.retirements.add(task);
    void task.finally(() => this.retirements.delete(task));
  }
  private constructor(readonly config: CitoConfig, readonly state: LocalState, readonly hostId: string, private readonly options: ServiceOptions) {
    for (const [name, hub] of Object.entries(config.hubs)) {
      const { stateDir, codexHome, codexBin, openBin, daemonPort, hostName, defaultHub, hubs, codexDesktopRunning } = config;
      const slotConfig = { stateDir, codexHome, codexBin, openBin, daemonPort, hostName, defaultHub, hubs, codexDesktopRunning, ...hub };
      this.slots.set(name, new HubSlot(name, slotConfig, options,
        broker => this.restoreReceivers(name, broker), () => {
          for (const receiver of this.receivers.values()) if (receiver.hub === name) { receiver.cancel(); void receiver.badge().catch(error => this.log(error)); }
        }, () => this.pushUiState()));
    }
  }
  private installReceiver(key: string, receiver: Receiver): void {
    this.receivers.set(key, receiver); this.state.liveReceivers.set(key, receiver);
    this.slots.get(receiver.hub)!.receivers.set(receiver.id, receiver);
  }
  startUi(): Promise<void> {
    if (this.closing) return Promise.resolve();
    return this.uiStarting ??= (async () => {
      this.ui ??= new UiBridge(this.config.stateDir, { ...this.options.ui, onAction: action => this.handleUiAction(action) });
      await this.ui.start(); this.pushUiState();
    })();
  }
  async connectBackground(): Promise<void> {
    await Promise.allSettled([...this.slots.values()].map(slot => slot.network({ interactive: false })));
    this.pushUiState();
  }
  pushUiState(): void {
    if (this.closing) return;
    const hubs = [...this.slots.values()].map(slot => ({ name: slot.name, status: slot.status,
      user: slot.principal?.owner_user ?? null, auth_mode: slot.config.authMode, error: slot.error }));
    const rows = this.state.db.prepare("SELECT * FROM local_listeners WHERE status = 'ACTIVE' ORDER BY hub, listener_id").all() as unknown as ListenerRow[];
    const listeners = rows.filter(row => {
      const receiver = this.receivers.get(`${row.hub}:${row.listener_id}`);
      return receiver && !receiver.abort.signal.aborted && receiver.owner === row.owner_token;
    }).map(row => ({ hub: row.hub, listener_id: row.listener_id, public_id: displayId(row), scoped_id: this.scoped(row.hub, displayId(row)),
      harness: row.harness, session_id: row.session_id, title: row.title, cwd: row.cwd, repo: row.repo, scope: row.scope,
      can_jump: getAdapter(row.harness)?.canJump === true }));
    this.ui?.state(hubs, listeners);
  }
  async handleUiAction(action: UiToDaemonAction): Promise<void> {
    if (action.op === "quit") { await this.close(); await this.options.onQuit?.(); return; }
    if (this.closing) return;
    const task = (async () => {
      switch (action.op) {
        case "unlisten": await this.unlistenById(action.hub, action.listener_id); break;
        case "unlisten_all": await this.unlistenAll(); break;
        case "set_scope": await this.setScope(action.hub, action.listener_id, action.scope); break;
        case "hub_action": {
          const slot = this.slot(action.hub);
          if (action.action === "disconnect") {
            await slot.disconnect();
            await Promise.allSettled([...slot.receivers.values()].map(receiver => receiver.badge()));
          }
          else await slot.reconnect(action.action === "login");
          break;
        }
      }
    })();
    this.calls.add(task);
    try { await task; } finally { this.calls.delete(task); this.pushUiState(); }
  }
  async unlistenById(hub: string, listenerId: string): Promise<ReturnType<typeof warning> | undefined> {
    const slot = this.slot(hub), id = toCanonicalListenerId(listenerId), key = `${hub}:${id}`;
    // Abort effects before waiting for their critical section; they hold the same mutex.
    const previous = this.receivers.get(key); previous?.cancel();
    const remoteWarning = await listenMutex.run(key, async () => {
      const row = this.state.listener(id, hub); if (!row) return;
      const receiver = this.receivers.get(key); receiver?.cancel();
      this.state.transaction(() => {
        this.state.run("UPDATE local_listeners SET status = 'UNLISTENED' WHERE hub = ? AND listener_id = ?", hub, id);
        this.state.run("UPDATE local_signals SET state = 'STORED_PASSIVE', held_reason = 'unlistened_before_dispatch' WHERE hub = ? AND listener_id = ? AND state IN ('RECEIVED','DISPATCHING')", hub, id);
      });
      await refreshSessionBadge(this.state, this.config, row.harness, row.session_id).catch(error => this.log(error));
      this.pushUiState();
      // Local revocation must succeed even when the hub is offline or manually disconnected.
      try {
        const broker = slot.connectedBroker(), entry = await broker?.kv.get(id);
        if (entry && broker) {
          const value = registrationSchema.parse(entry.json());
          if (value.owner_token === row.owner_token && value.binding_generation === row.binding_generation) await broker.kv.update(id,
            encode({ ...value, state: "UNLISTENED", binding_generation: row.binding_generation + 1 }), entry.revision);
        }
      } catch (error) { this.log(error); return warning(hub, error); }
    });
    await previous?.stop();
    if (this.receivers.get(key) === previous) { this.receivers.delete(key); this.state.liveReceivers.delete(key); slot.receivers.delete(id); }
    return remoteWarning;
  }
  async unlistenAll(): Promise<void> {
    const rows = this.state.db.prepare("SELECT hub, listener_id FROM local_listeners WHERE status IN ('ACTIVE','REBINDING')").all();
    await Promise.all(rows.map(row => this.unlistenById(String(row.hub), String(row.listener_id))));
  }
  async setScope(hub: string, listenerId: string, scope: "self" | "workspace"): Promise<void> {
    if (scope !== "self" && scope !== "workspace") throw new CitoError("INVALID_INPUT", "Invalid scope");
    const slot = this.slot(hub), broker = await slot.network(), id = toCanonicalListenerId(listenerId);
    await listenMutex.run(`${hub}:${id}`, async () => {
      const row = this.state.listener(id, hub), entry = await broker.kv.get(id), remote = entry && registrationSchema.parse(entry.json());
      if (!row || row.status !== "ACTIVE" || !remote || remote.state !== "ACTIVE" || row.owner_token !== remote.owner_token || row.binding_generation !== remote.binding_generation) throw new CitoError("STALE_GENERATION", "Listener ownership changed");
      await broker.kv.update(id, encode({ ...remote, scope }), entry!.revision);
      this.state.run("UPDATE local_listeners SET scope = ? WHERE hub = ? AND listener_id = ?", scope, hub, id);
      this.pushUiState();
    });
  }
  static async create(config = loadConfig(), options: ServiceOptions = {}, state?: LocalState): Promise<CitoService> {
    const hostId = await getHostId({ stateDir: config.stateDir });
    return new CitoService(config, state ?? await LocalState.open(config.stateDir), hostId, options);
  }
  private slot(hub?: string): HubSlot {
    const name = parseScopedId("_", this.config, hub).hub;
    return this.slots.get(name)!;
  }
  private targetSlots(hub?: string): HubSlot[] { return hub ? [this.slot(hub)] : [...this.slots.values()]; }
  private scoped(hub: string, id: string): string { return formatScopedId(hub, id, this.config); }
  private receiverOptions(): ReceiverOptions {
    return { ...this.options, onStateChanged: () => this.pushUiState(), onBanner: payload => { this.ui?.banner(payload); this.options.onBanner?.(payload); }, onSessionSwitched: (harness, sessionId) => this.revokeSession(harness, sessionId) };
  }
  private log(error: unknown) { (this.options.diagnostic ?? console.error)(String(error)); }
  private async restoreReceivers(hub: string, broker: Broker): Promise<void> {
    const slot = this.slots.get(hub)!;
    if (this.closing || slot.manuallyDisconnected) return;
    const fenced = this.state.db.prepare("SELECT * FROM local_listeners WHERE hub = ? AND status IN ('ACTIVE','REBINDING') AND principal_id != ?").all(hub, slot.principal!.principal_id) as unknown as ListenerRow[];
    for (const row of fenced) {
      this.receivers.get(`${hub}:${row.listener_id}`)?.cancel();
      this.state.run("UPDATE local_listeners SET status = 'FENCED_INACTIVE' WHERE hub = ? AND listener_id = ?", hub, row.listener_id);
      await refreshSessionBadge(this.state, this.config, row.harness, row.session_id);
    }
    const rows = this.state.db.prepare("SELECT * FROM local_listeners WHERE hub = ? AND status = 'ACTIVE'").all(hub) as unknown as ListenerRow[];
    for (const row of rows) {
      const id = row.listener_id, key = `${hub}:${id}`, previous = this.receivers.get(key);
      if (this.closing || (previous && !previous.abort.signal.aborted)) continue;
      await previous?.stop();
      let restored: Receiver | undefined;
      await listenMutex.run(key, async () => {
        const current = this.receivers.get(key);
        if (this.closing || slot.manuallyDisconnected || (current !== previous && current && !current.abort.signal.aborted)) return;
        const local = this.state.listener(id, hub); if (!local || local.status !== "ACTIVE") return;
        const entry = await broker.kv.get(id), remote = entry && registrationSchema.parse(entry.json());
        if (remote?.owner_token !== local.owner_token || remote.binding_generation !== local.binding_generation || remote.state !== "ACTIVE" || slot.manuallyDisconnected) return;
        if (local.scope !== remote.scope) { this.state.run("UPDATE local_listeners SET scope = ? WHERE hub = ? AND listener_id = ?", remote.scope, hub, id); local.scope = remote.scope; }
        this.state.recordOfflineGap(id, await brokerNow(broker.jsm), entry!.created, hub);
        restored = new Receiver(this.state, this.config, broker, local, this.receiverOptions());
        this.installReceiver(key, restored);
        if (!await restored.start()) { await restored.stop(); if (this.receivers.get(key) === restored) this.receivers.delete(key); restored = undefined; }
        else await restored.badge().catch(error => this.log(error));
      });
      this.pushUiState();
      if (restored) await restored.sweep();
    }
  }
  private cachedCaller(query: CachedSessionQuery): Caller | undefined {
    let rows: ListenerRow[];
    if (query.endpointPid !== undefined) {
      const pairs = this.state.db.prepare("SELECT DISTINCT harness, session_id FROM local_listeners WHERE endpoint_pid = ?").all(query.endpointPid);
      if (pairs.length > 1) throw new CitoError("PI_BRIDGE_UNAVAILABLE", "Ambiguous cached endpoint PID");
      if (!pairs.length) return;
      rows = this.state.db.prepare("SELECT * FROM local_listeners WHERE harness = ? AND session_id = ? ORDER BY binding_generation DESC, hub ASC, listener_id ASC")
        .all(pairs[0]!.harness!, pairs[0]!.session_id!) as unknown as ListenerRow[];
    } else rows = this.state.db.prepare("SELECT * FROM local_listeners WHERE harness = ? AND session_id = ? ORDER BY binding_generation DESC, hub ASC, listener_id ASC")
      .all(query.harness!, query.sessionId!) as unknown as ListenerRow[];
    const row = rows[0];
    return row && { harness: row.harness, session_id: row.session_id, endpoint_pid: row.endpoint_pid ?? undefined,
      cwd: row.cwd, title: row.title, repo: row.repo, branch: row.branch };
  }
  private caller(meta: Record<string, unknown>, open: boolean): Promise<Caller> {
    return resolveCaller(meta, this.config, { requireOpen: open, lookupCachedSession: query => this.cachedCaller(query) });
  }
  private async optionalCaller(meta: Record<string, unknown>): Promise<Caller | undefined> {
    try { return await this.caller(meta, false); }
    catch (error) {
      if (!(error instanceof CitoError) || !["BINDING_REQUIRED", "SESSION_NOT_OPEN"].includes(error.code)) throw error;
      return undefined;
    }
  }
  private sender(caller: Caller | undefined, principal: Principal, hub: string) {
    const endpoint = { principal_id: principal.principal_id, host_id: this.hostId, harness: caller?.harness ?? null, session_id: caller?.session_id ?? null };
    const local = caller && this.state.listener(listenerId({ ...endpoint, harness: caller.harness, session_id: caller.session_id }), hub);
    return { endpoint, caller, local, key: senderEndpointKey(endpoint) };
  }
  call(name: string, input: unknown = {}, meta: Record<string, unknown> = {}) {
    const task = (async () => {
      try {
        if (this.closing) throw new CitoError("NATS_UNAVAILABLE", "Service is closing");
        let result;
        switch (name) {
          case "cito_listen": result = await this.listen(parseInput(listenInputSchema, input), meta); break;
          case "cito_unlisten": result = await this.unlisten(parseInput(unlistenInputSchema, input), meta); break;
          case "cito_find": result = await this.find(parseInput(findInputSchema, input), meta); break;
          case "cito_send": result = await this.send(parseInput(sendInputSchema, input), meta); break;
          case "cito_inbox": result = await this.inbox(parseInput(inboxInputSchema, input), meta); break;
          default: throw new CitoError("INVALID_INPUT", "Unknown tool: " + name);
        }
        return successResult(result);
      } catch (error) {
        if (error instanceof CitoError) return errorResult(error.code, error.message);
        this.log(error); return errorResult("NATS_UNAVAILABLE", "Cito operation failed; inspect server stderr");
      }
    })();
    this.calls.add(task); void task.finally(() => this.calls.delete(task)); return task;
  }
  private async listen(input: z.infer<typeof listenInputSchema>, meta: Record<string, unknown>) {
    const slot = this.slot(input.hub), hub = slot.name;
    const caller = await this.caller(meta, true), broker = await slot.network(), principal = await slot.identity();
    const normalizedTag = input.tag === undefined ? undefined : normalizeSlug(input.tag);
    if (input.tag !== undefined && !normalizedTag) throw new CitoError("INVALID_INPUT", "tag must contain letters or numbers");
    const id = listenerId({ ...principal, host_id: this.hostId, harness: caller.harness, session_id: caller.session_id }), key = `${hub}:${id}`;
    return listenMutex.run(key, async () => {
    if (this.closing || slot.manuallyDisconnected) throw new CitoError("NATS_UNAVAILABLE", "Hub disconnected");
    let previous: Receiver | undefined, ownedToken: string | undefined, rebinding = false;
    const checkCollision = (existing: Registration | ListenerRow | null | undefined) => {
      if (existing && (existing.principal_id !== principal.principal_id || existing.host_id !== this.hostId || existing.harness !== caller.harness || existing.session_id !== caller.session_id)) {
        throw new CitoError("STALE_GENERATION", "Listener ID collision");
      }
    };
    try {
      const existing = await broker.kv.get(id); checkCollision(existing && registrationSchema.parse(existing.json()));
      const local = this.state.listener(id, hub); checkCollision(local);
      const tag = normalizedTag ?? local?.tag ?? null, publicId = publicListenerId(listenerSlug({ ...caller, tag }), id);
      previous = this.receivers.get(key); previous?.cancel(); rebinding = true;
      this.state.run(`INSERT INTO local_listeners (hub, listener_id, principal_id, owner_user, host_id, host_name, harness,
        session_id, endpoint_pid, cwd, repo, branch, title, about, scope, tag, public_id, status)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'REBINDING')
        ON CONFLICT(hub, listener_id) DO UPDATE SET status = 'REBINDING'`, hub, id, principal.principal_id,
        principal.owner_user, this.hostId, this.config.hostName, caller.harness, caller.session_id, caller.endpoint_pid ?? null,
        caller.cwd, caller.repo, caller.branch, caller.title, input.about, input.scope ?? "self", tag, publicId);
      const owner = randomUUID();
      for (let attempt = 0; attempt < 3; attempt++) {
        const entry = await broker.kv.get(id), row = this.state.listener(id, hub)!, remote = entry && registrationSchema.parse(entry.json());
        checkCollision(remote);
        if (entry) this.state.recordOfflineGap(id, await brokerNow(broker.jsm), entry.created, hub);
        const generation = Math.max(remote?.binding_generation ?? 0, row.binding_generation) + 1;
        const next = { ...registration(row), ...principal, host_name: this.config.hostName, cwd: caller.cwd, repo: caller.repo,
          branch: caller.branch, title: caller.title, about: input.about, tag, public_id: publicId,
          scope: input.scope ?? row.scope, state: "ACTIVE" as const, binding_generation: generation, owner_token: owner };
        try { if (entry) await broker.kv.update(id, encode(next), entry.revision); else await broker.kv.create(id, encode(next)); }
        catch (error) { if (isJsApiError(error, 10071)) continue; throw error; }
        this.state.transaction(() => {
          // Endpoint PIDs describe the host conversation, not one hub generation.
          this.state.run("UPDATE local_listeners SET endpoint_pid = ? WHERE harness = ? AND session_id = ?",
            caller.endpoint_pid ?? null, caller.harness, caller.session_id);
          this.state.run(`UPDATE local_listeners SET status = 'ACTIVE', binding_generation = ?, owner_token = ?, endpoint_pid = ?,
            cwd = ?, repo = ?, branch = ?, title = ?, about = ?, scope = ?, host_name = ?, tag = ?, public_id = ?,
            consecutive_probe_failures = 0, dispatcher_token = NULL, lease_until_ms = 0 WHERE hub = ? AND listener_id = ?`,
            generation, owner, caller.endpoint_pid ?? null, caller.cwd, caller.repo, caller.branch, caller.title, input.about,
            next.scope, this.config.hostName, tag, publicId, hub, id);
          this.state.run("UPDATE local_signals SET binding_generation = ?, dispatcher_token = NULL WHERE hub = ? AND listener_id = ? AND state IN ('RECEIVED','DISPATCHING')", generation, hub, id);
        });
        ownedToken = owner;
        if (this.closing || slot.manuallyDisconnected) throw new CitoError("NATS_UNAVAILABLE", "Hub disconnected during enrollment");
        const receiver = new Receiver(this.state, this.config, broker, this.state.listener(id, hub)!, this.receiverOptions());
        this.installReceiver(key, receiver);
        if (!await receiver.start()) { receiver.cancel(); throw new CitoError("NATS_UNAVAILABLE", "Receiver ownership lock unavailable"); }
        await receiver.badge().catch(error => this.log(error));
        return { ok: true, status: "listening", hub, listener_id: this.scoped(hub, publicId), binding_generation: generation,
          user: principal.owner_user, host_id: this.hostId, host_name: this.config.hostName, harness: caller.harness,
          session_id: caller.session_id, about: input.about, repo: caller.repo, branch: caller.branch, title: caller.title,
          scope: next.scope, lease_ttl_seconds: 60 };
      }
      throw new CitoError("STALE_GENERATION", "Listener CAS retries exhausted");
    } catch (error) {
      if (rebinding && !ownedToken) this.state.run("UPDATE local_listeners SET status = 'FENCED_INACTIVE' WHERE hub = ? AND listener_id = ?", hub, id);
      throw error;
    } finally { this.pushUiState(); this.retire(previous); }
    });
  }
  private async unlistenOne(slot: HubSlot, caller: Caller, principal: Principal) {
    const id = listenerId({ ...principal, host_id: this.hostId, harness: caller.harness, session_id: caller.session_id });
    const row = this.state.listener(id, slot.name);
    if (!row || row.status !== "ACTIVE") return { ok: true, status: "not_listening", listener_id: null, warnings: [] };
    const remoteWarning = await this.unlistenById(slot.name, id);
    return { ok: true, status: "unlistened", listener_id: this.scoped(slot.name, displayId(row)), warnings: remoteWarning ? [remoteWarning] : [] };
  }
  private async unlisten(input: z.infer<typeof unlistenInputSchema>, meta: Record<string, unknown>) {
    if (input.hub) this.slot(input.hub);
    const caller = await this.caller(meta, false);
    let slots = this.targetSlots(input.hub);
    if (!input.hub) {
      const active = [];
      for (const slot of slots) {
        try {
          const principal = await slot.identity(true), id = listenerId({ ...principal, host_id: this.hostId, harness: caller.harness, session_id: caller.session_id });
          if (this.state.listener(id, slot.name)?.status === "ACTIVE") active.push(slot);
        } catch (error) { if (!(error instanceof CitoError) || error.code !== "UNAUTHORIZED") throw error; }
      }
      slots = active.length ? active : [this.slot()];
    }
    const results = await Promise.allSettled(slots.map(async slot => this.unlistenOne(slot, caller, await slot.identity(true))));
    const successes = results.flatMap(r => r.status === "fulfilled" ? [r.value] : []);
    if (!successes.length) throw (results[0] as PromiseRejectedResult).reason;
    const warnings = results.flatMap((r, i) => r.status === "rejected" ? [warning(slots[i]!.name, r.reason)] : r.value.warnings);
    return { ok: true, status: successes.some(r => r.status === "unlistened") ? "unlistened" : "not_listening",
      listener_id: successes.find(r => r.listener_id)?.listener_id ?? null, ...(warnings.length ? { warnings } : {}) };
  }
  private async find(input: z.infer<typeof findInputSchema>, meta: Record<string, unknown>) {
    const slots = this.targetSlots(input.hub);
    // Resolve once before any hub operation so metadata conflicts cannot be hidden by partial success.
    const caller = await this.optionalCaller(meta);
    const results = await Promise.allSettled(slots.map(async slot => {
      const broker = await slot.network(), principal = await slot.identity(), sender = this.sender(caller, principal, slot.name), now = await brokerNow(broker.jsm);
      const keys = await broker.kv.keys(), names: string[] = []; for await (const key of keys) names.push(key);
      const found: { hub: string; value: Registration; revision: number; created: string }[] = [];
      const lower = (text: string | null) => (text ?? "").toLowerCase();
      for (const key of names) {
        const entry = await broker.kv.get(key); if (!entry || entry.operation !== "PUT") continue;
        let value: Registration;
        try { value = registrationSchema.parse(entry.json()); } catch { continue; }
        if (evaluateLease(value.state, now, entry.created) !== "ACTIVE" || (value.scope !== "workspace" && value.principal_id !== principal.principal_id)) continue;
        if (input.user && lower(value.owner_user) !== lower(input.user)) continue;
        if (input.host && value.host_id !== input.host && !lower(value.host_name).includes(lower(input.host))) continue;
        if (input.harness && value.harness !== input.harness) continue;
        if (input.repo && !lower(value.repo).includes(lower(input.repo))) continue;
        const haystack = [value.about, value.repo, value.branch, value.title, value.owner_user, value.host_name].map(lower).join(" ");
        if (input.query && !input.query.trim().split(/\s+/).every(word => haystack.includes(lower(word)))) continue;
        found.push({ hub: slot.name, value, revision: entry.revision, created: entry.created.toISOString() });
      }
      return { found, caller: { hub: slot.name, user: principal.owner_user, host_id: this.hostId, host_name: this.config.hostName,
        harness: sender.endpoint.harness, session_id: sender.endpoint.session_id,
        listener_id: sender.local?.status === "ACTIVE" ? this.scoped(slot.name, displayId(sender.local)) : null } };
    }));
    const successes = results.flatMap(r => r.status === "fulfilled" ? [r.value] : []);
    if (!successes.length) throw (results[0] as PromiseRejectedResult).reason;
    const found = successes.flatMap(r => r.found).sort((a, b) => b.revision - a.revision || a.hub.localeCompare(b.hub) || a.value.listener_id.localeCompare(b.value.listener_id));
    const warnings = results.flatMap((r, i) => r.status === "rejected" ? [warning(slots[i]!.name, r.reason)] : []);
    return { ok: true, caller: successes[0]!.caller, listeners: found.slice(0, input.limit).map(({ hub, value, created }) => ({
      hub, listener_id: this.scoped(hub, displayId(value)), user: value.owner_user, host_id: value.host_id, host_name: value.host_name,
      harness: value.harness, about: value.about, repo: value.repo, branch: value.branch, title: value.title, scope: value.scope, last_seen_at: created,
    })), ...(warnings.length ? { warnings } : {}) };
  }
  private sentResult(row: SentRow, to: string) {
    return { ok: true, signal_id: this.scoped(row.hub, row.signal_id), seq: row.seq, to: this.scoped(row.hub, to),
      status: row.status, held_reason: row.held_reason, expires_at: row.expires_at_iso };
  }
  private async send(input: z.infer<typeof sendInputSchema>, meta: Record<string, unknown>) {
    const { hub, id } = parseScopedId(input.to, this.config, input.hub);
    const replyId = input.in_reply_to === undefined ? null : parseScopedId(input.in_reply_to, this.config, hub).id;
    const canonicalTo = toCanonicalListenerId(id), slot = this.slot(hub);
    // Claim validation precedes network/DB effects.
    const caller = await this.optionalCaller(meta);
    const broker = await slot.network(), identity = await slot.identity();
    const from = this.sender(caller, identity, hub);
    const entry = await broker.kv.get(canonicalTo), now = await brokerNow(broker.jsm);
    if (!entry) throw new CitoError("LISTENER_NOT_FOUND", "Target listener does not exist");
    const target = registrationSchema.parse(entry.json()), lease = evaluateLease(target.state, now, entry.created);
    if (lease === "UNLISTENED") throw new CitoError("LISTENER_NOT_FOUND", "Target listener was revoked");
    if (lease === "EXPIRED") throw new CitoError("LISTENER_EXPIRED", "Target listener expired");
    if (from.endpoint.principal_id === target.principal_id && from.endpoint.host_id === target.host_id && from.endpoint.harness === target.harness && from.endpoint.session_id === target.session_id) {
      throw new CitoError("SELF_SEND_FORBIDDEN", "Cannot send to your own session");
    }
    if (identity.principal_id !== target.principal_id && target.scope !== "workspace") throw new CitoError("FORBIDDEN_TARGET_SCOPE", "Target is private to its principal");
    const dedup = sha256(JSON.stringify([from.key, canonicalTo, input.idempotency_key ?? randomUUID()]));
    const hash = sha256(JSON.stringify([input.message, replyId]));
    const reservation = this.state.transaction(() => {
      this.state.run("DELETE FROM local_sent_idempotency WHERE expires_at_ms <= ?", Date.now());
      const previous = this.state.db.prepare("SELECT * FROM local_sent_idempotency WHERE hub = ? AND dedup_id = ?").get(hub, dedup) as unknown as SentRow | undefined;
      if (previous) { if (previous.request_hash !== hash) throw new CitoError("IDEMPOTENCY_KEY_CONFLICT", "Idempotency key was used with another payload"); return previous; }
      const expiry = Date.now() + RETENTION_MS;
      this.state.run(`INSERT INTO local_sent_idempotency (hub, dedup_id, request_hash, signal_id, to_listener_id, status, expires_at_iso, expires_at_ms)
        VALUES (?, ?, ?, ?, ?, 'PENDING_PUBLISH', ?, ?)`, hub, dedup, hash, "sig_" + randomUUID().replaceAll("-", ""), canonicalTo, new Date(expiry).toISOString(), expiry);
      return this.state.db.prepare("SELECT * FROM local_sent_idempotency WHERE hub = ? AND dedup_id = ?").get(hub, dedup) as unknown as SentRow;
    });
    if (reservation.status !== "PENDING_PUBLISH") return this.sentResult(reservation, displayId(target));
    let accept!: (value: z.infer<typeof ackSchema> | null) => void;
    const ack = new Promise<z.infer<typeof ackSchema> | null>(resolve => { accept = resolve; });
    const sub = broker.nc.subscribe("cito.ack." + reservation.signal_id, { callback: (error, msg) => {
      if (error) return;
      try { const reply = ackSchema.parse(msg.json()); if (reply.signal_id === reservation.signal_id) accept(reply); } catch { /* Timeout preserves publish milestone. */ }
    } });
    const timer = setTimeout(() => accept(null), 2000), operation = AbortSignal.any([this.shuttingDown.signal, AbortSignal.timeout(10_000)]);
    let interrupt!: () => void;
    const interrupted = new Promise<never>((_, reject) => {
      interrupt = () => reject(new CitoError("NATS_UNAVAILABLE", "Send interrupted or NATS operation deadline exceeded"));
      operation.addEventListener("abort", interrupt, { once: true }); if (operation.aborted) interrupt();
    });
    try {
      await Promise.race([broker.nc.flush(), interrupted]);
      const published = await Promise.race([broker.js.publish("cito.signal." + canonicalTo, encode({ signal_id: reservation.signal_id,
        to: canonicalTo, target_generation: target.binding_generation, sender_endpoint_key: from.key,
        from: { principal_id: identity.principal_id, user: identity.owner_user, host_id: this.hostId, host_name: this.config.hostName,
          harness: from.endpoint.harness, listener_id: from.local?.status === "ACTIVE" ? displayId(from.local) : null,
          title: from.caller?.title ?? "", about: from.local?.about ?? null }, message: input.message, in_reply_to: replyId }), { msgID: dedup }), interrupted]);
      const reply = await Promise.race([ack, interrupted]);
      this.state.run("UPDATE local_sent_idempotency SET seq = ?, status = ?, held_reason = ? WHERE hub = ? AND dedup_id = ? AND status = 'PENDING_PUBLISH'",
        published.seq, reply?.status ?? "queued_for_reconnect", reply?.held_reason ?? null, hub, dedup);
      return this.sentResult(this.state.db.prepare("SELECT * FROM local_sent_idempotency WHERE hub = ? AND dedup_id = ?").get(hub, dedup) as unknown as SentRow, displayId(target));
    } finally { clearTimeout(timer); sub.unsubscribe(); operation.removeEventListener("abort", interrupt); }
  }
  private async inbox(input: z.infer<typeof inboxInputSchema>, meta: Record<string, unknown>) {
    const slots = this.targetSlots(input.hub);
    const parsedAcks = input.ack_ids?.map(raw => ({ raw, ...parseScopedId(raw, this.config, input.hub) }));
    const caller = await this.caller(meta, false);
    const authorized: { hub: string; id: string }[] = [];
    let authError: unknown;
    for (const slot of slots) {
      try {
        const principal = await slot.identity(true);
        authorized.push({ hub: slot.name, id: listenerId({ ...principal, host_id: this.hostId, harness: caller.harness, session_id: caller.session_id }) });
      } catch (error) {
        if (!(error instanceof CitoError) || error.code !== "UNAUTHORIZED" || slots.length === 1) throw error;
        authError = error;
      }
    }
    if (!authorized.length) throw authError;
    const ackAll = input.ack_all ?? (input.ack_ids === undefined), now = Date.now();
    const result = this.state.transaction(() => {
      const where = authorized.map(() => "(hub = ? AND listener_id = ?)").join(" OR ");
      const values = authorized.flatMap(r => [r.hub, r.id]);
      const ackRows: SignalRow[] = [];
      for (const ack of parsedAcks ?? []) {
        const scoped = ack.raw.includes(":");
        if (scoped && !authorized.some(row => row.hub === ack.hub)) throw new CitoError("INVALID_INPUT", "Ack hub is outside the authorized inbox");
        const matches = this.state.signals(`SELECT * FROM local_signals WHERE (${where}) AND signal_id = ?${scoped ? " AND hub = ?" : ""}`, ...values, ack.id, ...(scoped ? [ack.hub] : []));
        if (!scoped && new Set(matches.map(row => row.hub)).size > 1) throw new CitoError("INVALID_INPUT", "Ambiguous bare signal ID; use a hub prefix");
        ackRows.push(...matches);
      }
      const listeners = authorized.map(r => this.state.listener(r.id, r.hub)).filter((r): r is ListenerRow => !!r);
      const rows = this.state.signals(`SELECT * FROM local_signals WHERE (${where}) AND acknowledged_at_ms IS NULL AND expires_at_ms > ?
        AND state != 'EXPIRED_UNSENT' ORDER BY created_at ASC, hub ASC, seq ASC, signal_id ASC LIMIT 50`, ...values, now);
      const ids = new Map([...ackRows, ...(ackAll ? rows : [])].map(row => [`${row.hub}:${row.signal_id}`, row]));
      let count = 0;
      for (const row of ids.values()) count += Number(this.state.run(`UPDATE local_signals SET acknowledged_at_ms = COALESCE(acknowledged_at_ms, ?),
        state = CASE WHEN state = 'RECEIVED' THEN 'STORED_PASSIVE' ELSE state END WHERE hub = ? AND listener_id = ? AND signal_id = ? AND acknowledged_at_ms IS NULL`, now, row.hub, row.listener_id, row.signal_id).changes);
      if (ackAll) for (const row of authorized) this.state.run("UPDATE local_listeners SET replay_gap_detected = 0, replay_gap_range = NULL WHERE hub = ? AND listener_id = ?", row.hub, row.id);
      const first = listeners[0], fallback = authorized[0]!;
      return { ok: true, listener_id: first ? this.scoped(first.hub, displayId(first)) : this.scoped(fallback.hub, publicListenerId(listenerSlug(caller), fallback.id)),
        replay_gap_detected: listeners.some(r => !!r.replay_gap_detected), replay_gap_range: listeners.find(r => r.replay_gap_range)?.replay_gap_range ?? null,
        signals: rows.map(row => ({ signal_id: this.scoped(row.hub, row.signal_id), hub: row.hub, seq: row.seq,
          from: { user: row.from_user, host_name: row.from_host_name, harness: row.from_harness,
            listener_id: row.from_listener_id ? this.scoped(row.hub, row.from_listener_id.startsWith("c_") ? publicListenerId(normalizeSlug(row.from_title) || row.from_harness || "listener", row.from_listener_id) : row.from_listener_id) : null,
            about: row.from_about }, message: row.message, held_reason: row.held_reason, in_reply_to: row.in_reply_to ? this.scoped(row.hub, row.in_reply_to) : null, created_at: row.created_at })), acknowledged_count: count };
    });
    await refreshSessionBadge(this.state, this.config, caller.harness, caller.session_id).catch(error => this.log(error));
    return result;
  }
  private async revokeSession(harness: string, sessionId: string): Promise<void> {
    const rows = this.state.db.prepare("SELECT * FROM local_listeners WHERE harness = ? AND session_id = ? AND status = 'ACTIVE'").all(harness, sessionId) as unknown as ListenerRow[];
    // Fence every local binding before any potentially failing remote update.
    for (const row of rows) this.receivers.get(`${row.hub}:${row.listener_id}`)?.cancel();
    this.state.transaction(() => {
      for (const row of rows) {
        this.state.run("UPDATE local_listeners SET status = 'UNLISTENED' WHERE hub = ? AND listener_id = ?", row.hub, row.listener_id);
        this.state.run("UPDATE local_signals SET state = 'STORED_PASSIVE', held_reason = 'session_switched' WHERE hub = ? AND listener_id = ? AND state IN ('RECEIVED','DISPATCHING')", row.hub, row.listener_id);
      }
    });
    await Promise.allSettled(rows.map(async row => {
      const broker = await this.slot(row.hub).network(), entry = await broker.kv.get(row.listener_id);
      if (!entry) return;
      const value = registrationSchema.parse(entry.json());
      if (value.owner_token === row.owner_token && value.binding_generation === row.binding_generation) await broker.kv.update(row.listener_id,
        encode({ ...value, state: "UNLISTENED", binding_generation: row.binding_generation + 1 }), entry.revision);
    }));
    await refreshSessionBadge(this.state, this.config, harness, sessionId).catch(error => this.log(error));
  }
  close(): Promise<void> { return this.closePromise ??= this.shutdown(); }
  private async shutdown(): Promise<void> {
    this.closing = true; this.shuttingDown.abort();
    // Mark the bridge stopped before waiting for a cold compilation to finish.
    const uiClosing = this.ui?.close();
    const closingKeys = new Set(this.receivers.keys()), receivers = [...this.receivers.values()];
    for (const receiver of receivers) receiver.cancel();
    const slotsClosing = Promise.allSettled([...this.slots.values()].map(slot => slot.close()));
    await Promise.allSettled([...this.calls]);
    for (const receiver of this.receivers.values()) if (!receivers.includes(receiver)) receivers.push(receiver);
    for (const receiver of receivers) { receiver.cancel(); closingKeys.add(`${receiver.hub}:${receiver.id}`); }
    await Promise.allSettled([...receivers.map(receiver => receiver.stop()), ...this.retirements]);
    const sessions = new Map<string, ListenerRow>();
    for (const receiver of receivers) {
      const row = this.state.listener(receiver.id, receiver.hub); if (row) sessions.set(`${row.harness}:${row.session_id}`, row);
    }
    for (const row of sessions.values()) await refreshSessionBadge(this.state, this.config, row.harness, row.session_id, { closingKeys }).catch(error => this.log(error));
    this.receivers.clear(); this.state.liveReceivers.clear(); await slotsClosing;
    await uiClosing; await this.uiStarting?.catch(error => this.log(error)); this.state.close();
  }
}
