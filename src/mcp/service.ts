import { randomUUID } from "node:crypto";
import { z } from "zod";
import { Events, ErrorCode } from "nats";
import { acquireFlock } from "../banner/flock.js";
import { codexThreadId, resolveCodexCaller, resolvePiCaller, readCodexThread, setPiStatus,
  type CodexCaller, type PiCaller } from "../adapters/index.js";
import { CitoError, brokerNow, evaluateLease, RETENTION_MS, isJsApiError, connectNats, provisionNats, loadConfig, getHostId,
  localPrincipal, listenerId, listenerSlug, normalizeSlug, publicListenerId, toCanonicalListenerId,
  senderEndpointKey, parseInput, successResult, errorResult,
  listenInputSchema, unlistenInputSchema, findInputSchema, sendInputSchema, inboxInputSchema,
  type CitoConfig, type Principal } from "../shared/index.js";
import { LocalState, registration, registrationSchema, type ListenerRow, type Registration, type SentRow } from "./state.js";
import { OidcLogin, sha256, type BrowserOpener } from "./oidc.js";
import { Receiver, encode, listenLock, refreshPiBadge, type Broker, type ReceiverOptions } from "./receiver.js";

type Caller = CodexCaller | PiCaller;
export interface ServiceOptions extends ReceiverOptions { browserOpener?: BrowserOpener }
const ackSchema = z.object({ signal_id: z.string(), effective_wake: z.boolean(),
  status: z.enum(["wake_accepted", "stored_passive"]), held_reason: z.string().nullable() });
const displayId = (row: ListenerRow | Registration): string =>
  row.public_id || publicListenerId(listenerSlug(row), row.listener_id);
function sentResult(row: SentRow, to: string) {
  return { ok: true, signal_id: row.signal_id, seq: row.seq, to, requested_wake: !!row.requested_wake,
    effective_wake: !!row.effective_wake, status: row.status, held_reason: row.held_reason, expires_at: row.expires_at_iso };
}

/** Lazy NATS connection keeps inbox usable without a broker or browser login. Close after use. */
export class CitoService {
  readonly receivers = new Map<string, Receiver>();
  private brokerPromise?: Promise<Broker>;
  private broker?: Broker;
  private rejectedHash?: string;
  private readonly background = new Set<Promise<unknown>>();
  private readonly login?: OidcLogin;
  private principal?: Principal;
  private closing = false;
  private readonly shuttingDown = new AbortController();
  private closePromise?: Promise<void>;
  private readonly calls = new Set<Promise<unknown>>();
  private constructor(readonly config: CitoConfig, readonly state: LocalState, readonly hostId: string,
    private readonly options: ServiceOptions) {
    if (config.authMode === "oidc") this.login = new OidcLogin(config, options.browserOpener);
    else this.principal = localPrincipal(config.user);
  }
  static async create(config = loadConfig(), options: ServiceOptions = {}): Promise<CitoService> {
    const hostId = await getHostId({ stateDir: config.stateDir });
    return new CitoService(config, await LocalState.open(config.stateDir), hostId, options);
  }
  private async network(): Promise<Broker> {
    if (this.closing) throw new CitoError("NATS_UNAVAILABLE", "Service is closing");
    if (this.broker && !this.broker.nc.isClosed() && !this.rejectedHash) {
      await this.restoreReceivers(this.broker);
      return this.broker;
    }
    if (this.brokerPromise) return this.brokerPromise;
    this.brokerPromise = (async () => {
      if (this.rejectedHash) await this.broker?.nc.close();
      let token = this.login ? (this.rejectedHash ? await this.login.recover(this.rejectedHash) : await this.login.current()) : undefined;
      this.rejectedHash = undefined;
      const connect = () => connectNats({ servers: this.config.natsUrl, token: token?.id_token,
        name: "cito-mcp", timeout: 10_000, maxReconnectAttempts: -1 });
      let nc;
      try { nc = await connect(); }
      catch (cause) {
        if (this.login && /authorization|authentication/i.test(String(cause))) {
          token = await this.login.recover(token?.token_sha256 ?? null);
          try { nc = await connect(); } catch (cause) { throw new CitoError("UNAUTHORIZED", "NATS rejected refreshed OIDC token", { cause }); }
        } else throw new CitoError("NATS_UNAVAILABLE", "Cannot connect to NATS", { cause });
      }
      try {
        const topology = await provisionNats(nc);
        this.broker = { ...topology, nc };
        if (token) this.principal = { principal_id: token.verified_principal_id, owner_user: token.verified_user };
        const broker = this.broker;
        const monitor = this.monitor(broker, token?.token_sha256);
        this.background.add(monitor);
        void monitor.catch(error => { if (!this.closing) (this.options.diagnostic ?? console.error)(String(error)); })
          .finally(() => this.background.delete(monitor));
        await this.restoreReceivers(broker);
        return broker;
      } catch (cause) { await nc.close(); throw new CitoError("NATS_UNAVAILABLE", "NATS topology unavailable", { cause }); }
    })().finally(() => { this.brokerPromise = undefined; });
    return this.brokerPromise;
  }
  private async monitor(broker: Broker, tokenHash?: string): Promise<void> {
    const statuses = broker.nc.status();
    // nats v2 does not terminate status iterators when the connection closes.
    if (!("stop" in statuses) || typeof statuses.stop !== "function") throw new Error("Unsupported NATS status iterator");
    const stopStatuses = statuses.stop.bind(statuses);
    void broker.nc.closed().then(error => {
      if (!this.closing && tokenHash && error && "code" in error && [ErrorCode.AuthorizationViolation, ErrorCode.AuthenticationExpired].includes(error.code as ErrorCode)) {
        this.rejectedHash = tokenHash;
        for (const receiver of this.receivers.values()) receiver.cancel();
      }
      stopStatuses();
    });
    for await (const status of statuses) {
      if (this.closing) break;
      if (status.type === Events.Disconnect) for (const receiver of this.receivers.values()) receiver.cancel();
      if (status.type === Events.Error && tokenHash
        && [ErrorCode.AuthorizationViolation, ErrorCode.AuthenticationExpired].includes(status.data as ErrorCode)) {
        this.rejectedHash = tokenHash;
        for (const receiver of this.receivers.values()) receiver.cancel();
        await broker.nc.close();
        break;
      }
      if (status.type === Events.Reconnect) await this.restoreReceivers(broker);
    }
  }
  private async restoreReceivers(broker: Broker): Promise<void> {
    const ids = new Set([
      ...this.state.db.prepare("SELECT listener_id FROM local_listeners WHERE status = 'ACTIVE'").all().map(row => String(row.listener_id)),
      ...this.receivers.keys(),
    ]);
    for (const id of ids) {
      const previous = this.receivers.get(id);
      if (this.closing || (previous && !previous.abort.signal.aborted)) continue;
      await previous?.stop();
      const lock = await acquireFlock(listenLock(this.config, id), 5000);
      if (!lock) continue;
      let restored: Receiver | undefined;
      try {
        const current = this.receivers.get(id);
        if (this.closing || (current !== previous && current && !current.abort.signal.aborted)) continue;
        const local = this.state.listener(id);
        if (!local || local.status !== "ACTIVE") continue;
        const entry = await broker.kv.get(id);
        const remote = entry && registrationSchema.parse(entry.json());
        if (remote?.owner_token !== local.owner_token || remote.binding_generation !== local.binding_generation || remote.state !== "ACTIVE") continue;
        this.state.recordOfflineGap(id, await brokerNow(broker.jsm), entry!.created);
        restored = new Receiver(this.state, this.config, broker, local, this.options);
        this.receivers.set(id, restored);
        if (!await restored.start(0)) {
          await restored.stop();
          if (this.receivers.get(id) === restored) this.receivers.delete(id);
          restored = undefined;
        } else await restored.badge().catch(error => (this.options.diagnostic ?? console.error)(String(error)));
      } finally { await lock.release(); }
      if (restored) await restored.sweep();
    }
  }
  private async identity(offline = false): Promise<Principal> {
    if (offline && this.login) return this.login.offlinePrincipal();
    if (!this.principal) await this.network();
    return this.principal!;
  }
  private async caller(meta: Record<string, unknown>, open: boolean, principal: Principal): Promise<Caller> {
    let threadId: string | undefined;
    try { threadId = codexThreadId(meta); } catch (error) { if (!(error instanceof CitoError) || error.code !== "BINDING_REQUIRED") throw error; }
    if (threadId) {
      if (open) return resolveCodexCaller(meta, this.config);
      const id = listenerId({ ...principal, host_id: this.hostId, harness: "codex", session_id: threadId });
      const local = this.state.listener(id);
      if (local) return { harness: "codex", session_id: threadId, cwd: local.cwd, title: local.title, repo: local.repo, branch: local.branch };
      return readCodexThread(threadId, this.config, { allowArchived: true });
    }
    return resolvePiCaller(meta, this.config);
  }
  private async outbound(meta: Record<string, unknown>, principal: Principal) {
    let caller: Caller | undefined;
    try { caller = await this.caller(meta, false, principal); } catch { /* Outbound routing never requires a receiver binding. */ }
    const endpoint = { principal_id: principal.principal_id, host_id: this.hostId,
      harness: caller?.harness ?? null, session_id: caller?.session_id ?? null };
    const local = caller && this.state.listener(listenerId({ ...endpoint, harness: caller.harness, session_id: caller.session_id }));
    return { endpoint, caller, local, key: senderEndpointKey(endpoint) };
  }
  call(name: string, input: unknown = {}, meta: Record<string, unknown> = {}) {
    const task = (async () => {
      try {
        if (this.closing) throw new CitoError("NATS_UNAVAILABLE", "Service is closing");
        let payload: unknown;
        switch (name) {
          case "cito_listen": payload = await this.listen(parseInput(listenInputSchema, input), meta); break;
          case "cito_unlisten": parseInput(unlistenInputSchema, input); payload = await this.unlisten(meta); break;
          case "cito_find": payload = await this.find(parseInput(findInputSchema, input), meta); break;
          case "cito_send": payload = await this.send(parseInput(sendInputSchema, input), meta); break;
          case "cito_inbox": payload = await this.inbox(parseInput(inboxInputSchema, input), meta); break;
          default: throw new CitoError("INVALID_INPUT", "Unknown Cito tool");
        }
        return successResult(payload);
      } catch (error) {
        if (error instanceof CitoError) return errorResult(error.code, error.message);
        (this.options.diagnostic ?? console.error)(`cito: ${error instanceof Error ? error.message : String(error)}`);
        return errorResult("NATS_UNAVAILABLE", "Cito operation failed; inspect server stderr");
      }
    })();
    this.calls.add(task);
    void task.finally(() => this.calls.delete(task));
    return task;
  }
  private async listen(input: z.infer<typeof listenInputSchema>, meta: Record<string, unknown>) {
    const normalizedTag = input.tag === undefined ? undefined : normalizeSlug(input.tag);
    if (normalizedTag === "") throw new CitoError("INVALID_INPUT", "tag must contain at least one ASCII letter or digit");
    const broker = await this.network(), principal = await this.identity();
    const caller = await this.caller(meta, true, principal);
    const id = listenerId({ ...principal, host_id: this.hostId, harness: caller.harness, session_id: caller.session_id });
    const lock = await acquireFlock(listenLock(this.config, id), 5000);
    if (!lock) throw new CitoError("STALE_GENERATION", "Listener takeover lock timeout");
    let previous: Receiver | undefined;
    let ownedToken: string | undefined;
    let rebinding = false;
    const checkCollision = (existing: Registration | ListenerRow | null | undefined) => {
      if (existing && (existing.principal_id !== principal.principal_id || existing.host_id !== this.hostId
        || existing.harness !== caller.harness || existing.session_id !== caller.session_id)) {
        throw new CitoError("STALE_GENERATION", "Listener ID collision");
      }
    };
    try {
      // Reject a truncated fingerprint collision before cancelling any owner or writing local state.
      const existing = await broker.kv.get(id);
      checkCollision(existing && registrationSchema.parse(existing.json()));
      const local = this.state.listener(id);
      checkCollision(local);
      const tag = normalizedTag ?? local?.tag ?? null;
      const publicId = publicListenerId(listenerSlug({ ...caller, tag }), id);
      previous = this.receivers.get(id); previous?.cancel();
      rebinding = true;
      this.state.transaction(() => {
        this.state.run(`INSERT INTO local_listeners (listener_id, principal_id, owner_user, host_id, host_name, harness,
          session_id, pi_pid, cwd, repo, branch, title, about, scope, allow_peer_wake, tag, public_id, status)
          VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'REBINDING')
          ON CONFLICT(listener_id) DO UPDATE SET status = 'REBINDING'`, id, principal.principal_id,
          principal.owner_user, this.hostId, this.config.hostName, caller.harness, caller.session_id,
          caller.harness === "pi" ? caller.pi_pid : null, caller.cwd, caller.repo, caller.branch, caller.title,
          input.about, input.scope ?? "self", +(input.allow_peer_wake ?? false), tag, publicId);
        if (caller.harness === "pi") this.state.observePi(id, caller.bridge_epoch, caller.human_event_count);
        this.state.consumeHuman(id);
      });
      const owner = randomUUID();
      for (let attempt = 0; attempt < 3; attempt++) {
        const entry = await broker.kv.get(id), local = this.state.listener(id)!;
        const remote = entry && registrationSchema.parse(entry.json());
        checkCollision(remote);
        if (entry) this.state.recordOfflineGap(id, await brokerNow(broker.jsm), entry.created);
        const generation = Math.max(remote?.binding_generation ?? 0, local.binding_generation) + 1;
        const next = { ...registration(local), ...principal, host_name: this.config.hostName,
          cwd: caller.cwd, repo: caller.repo, branch: caller.branch, title: caller.title, about: input.about,
          tag, public_id: publicId,
          scope: input.scope ?? local.scope, allow_peer_wake: input.allow_peer_wake ?? !!local.allow_peer_wake,
          state: "ACTIVE" as const, binding_generation: generation, owner_token: owner };
        try { if (entry) await broker.kv.update(id, encode(next), entry.revision); else await broker.kv.create(id, encode(next)); }
        catch (error) { if (isJsApiError(error, 10071)) continue; throw error; }
        this.state.transaction(() => {
          this.state.run(`UPDATE local_listeners SET status = 'ACTIVE', binding_generation = ?, owner_token = ?,
            pi_pid = ?, cwd = ?, repo = ?, branch = ?, title = ?, about = ?, scope = ?, allow_peer_wake = ?,
            host_name = ?, tag = ?, public_id = ?, consecutive_probe_failures = 0, dispatcher_token = NULL, lease_until_ms = 0
            WHERE listener_id = ?`, generation, owner, caller.harness === "pi" ? caller.pi_pid : null,
            caller.cwd, caller.repo, caller.branch, caller.title, input.about, next.scope, +next.allow_peer_wake,
            this.config.hostName, tag, publicId, id);
          // Recovery transfers admitted work to the new owner without changing frozen membership or batch_id.
          this.state.run(`UPDATE local_signals SET binding_generation = ?, dispatcher_token = NULL
            WHERE listener_id = ? AND state IN ('RECEIVED','DISPATCHING')`, generation, id);
        });
        ownedToken = owner;
        const receiver = new Receiver(this.state, this.config, broker, this.state.listener(id)!, this.options);
        this.receivers.set(id, receiver);
        if (!await receiver.start()) { receiver.cancel(); throw new CitoError("NATS_UNAVAILABLE", "Receiver ownership lock unavailable"); }
        await receiver.badge().catch(error => (this.options.diagnostic ?? console.error)(String(error)));
        return { ok: true, status: "listening", listener_id: publicId, binding_generation: generation,
          user: principal.owner_user, host_id: this.hostId, host_name: this.config.hostName, harness: caller.harness,
          session_id: caller.session_id, about: input.about, repo: caller.repo, branch: caller.branch, title: caller.title,
          scope: next.scope, allow_peer_wake: next.allow_peer_wake, unattended_wakes_remaining: next.unattended_wakes_remaining,
          lease_ttl_seconds: 60 };
      }
      throw new CitoError("STALE_GENERATION", "Listener CAS retries exhausted");
    } catch (error) {
      if (rebinding && !ownedToken) this.state.run("UPDATE local_listeners SET status = 'FENCED_INACTIVE' WHERE listener_id = ?", id);
      throw error;
    } finally {
      await lock.release();
      await previous?.stop();
    }
  }
  private async unlisten(meta: Record<string, unknown>) {
    const principal = await this.identity(true), caller = await this.caller(meta, false, principal);
    const id = listenerId({ ...principal, host_id: this.hostId, harness: caller.harness, session_id: caller.session_id });
    if (this.state.listener(id)?.status !== "ACTIVE") return { ok: true, status: "not_listening", listener_id: null };
    // Establish connectivity before pausing effects. Failure here leaves the
    // active receiver untouched; later failures restore its persisted owner.
    const broker = await this.network();
    const previous = this.receivers.get(id);
    previous?.cancel(); // Abort an in-flight queue before waiting for its effect lock.
    let receiver = previous;
    let revoked = false;
    let lock: Awaited<ReturnType<typeof acquireFlock>> = null;
    try {
      lock = await acquireFlock(listenLock(this.config, id), 5000);
      if (!lock) throw new CitoError("STALE_GENERATION", "Listener revocation lock timeout");
      const row = this.state.listener(id);
      if (!row || row.status !== "ACTIVE") return { ok: true, status: "not_listening", listener_id: null };
      receiver = this.receivers.get(id);
      const pending = this.state.signals("SELECT * FROM local_signals WHERE listener_id = ? AND state IN ('RECEIVED','DISPATCHING')", id);
      const entry = await broker.kv.get(id);
      if (entry) {
        const value = registrationSchema.parse(entry.json());
        await broker.kv.update(id, encode({ ...value, state: "UNLISTENED", binding_generation: Math.max(value.binding_generation, row.binding_generation) + 1 }), entry.revision);
      }
      this.state.transaction(() => {
        this.state.run("UPDATE local_listeners SET status = 'UNLISTENED' WHERE listener_id = ?", id);
        // A remote owner's KV watcher may already have fenced these exact rows.
        for (const signal of pending) this.state.run(`UPDATE local_signals SET state = 'STORED_PASSIVE', held_reason = 'unlistened_before_dispatch'
          WHERE listener_id = ? AND signal_id = ?`, id, signal.signal_id);
      });
      revoked = true;
      receiver?.cancel();
      if (this.state.listener(id)?.owner_token === row.owner_token && row.harness === "pi" && row.pi_pid) await setPiStatus({ pi_pid: row.pi_pid, session_id: row.session_id }, undefined, this.config).catch(error => (this.options.diagnostic ?? console.error)(String(error)));
      return { ok: true, status: "unlistened", listener_id: displayId(row) };
    } finally {
      await lock?.release();
      await previous?.stop();
      if (revoked || this.state.listener(id)?.status !== "ACTIVE") {
        await receiver?.stop();
        if (this.receivers.get(id) === receiver) this.receivers.delete(id);
      } else {
        await this.restoreReceivers(broker).catch(error => (this.options.diagnostic ?? console.error)(String(error)));
      }
    }
  }
  private async find(input: z.infer<typeof findInputSchema>, meta: Record<string, unknown>) {
    const broker = await this.network(), principal = await this.identity();
    const sender = await this.outbound(meta, principal), now = await brokerNow(broker.jsm);
    const keys = await broker.kv.keys();
    const names: string[] = [];
    // Drain the nats v2 snapshot before doing I/O: stopping its iterator can
    // otherwise discard keys queued while a per-key get is awaiting a reply.
    for await (const key of keys) names.push(key);
    const results = [];
    const lower = (text: string | null) => (text ?? "").toLowerCase();
    for (const key of names) {
      const entry = await broker.kv.get(key); if (!entry || entry.operation !== "PUT") continue;
      const value = registrationSchema.parse(entry.json());
      if (evaluateLease(value.state, now, entry.created) !== "ACTIVE") continue;
      if (value.scope !== "workspace" && value.principal_id !== principal.principal_id) continue;
      if (input.user && lower(value.owner_user) !== lower(input.user)) continue;
      if (input.host && value.host_id !== input.host && !lower(value.host_name).includes(lower(input.host))) continue;
      if (input.harness && value.harness !== input.harness) continue;
      if (input.repo && !lower(value.repo).includes(lower(input.repo))) continue;
      const haystack = [value.about, value.repo, value.branch, value.title, value.owner_user, value.host_name].map(lower).join(" ");
      if (input.query && !input.query.trim().split(/\s+/).every(word => haystack.includes(lower(word)))) continue;
      results.push({ value, revision: entry.revision, created: entry.created.toISOString() });
    }
    results.sort((a, b) => b.revision - a.revision || a.value.listener_id.localeCompare(b.value.listener_id));
    return { ok: true, caller: { user: principal.owner_user, host_id: this.hostId, host_name: this.config.hostName,
      harness: sender.endpoint.harness, session_id: sender.endpoint.session_id,
      listener_id: sender.local?.status === "ACTIVE" ? displayId(sender.local) : null },
    listeners: results.slice(0, input.limit).map(({ value, created }) => ({ listener_id: displayId(value),
      user: value.owner_user, host_id: value.host_id, host_name: value.host_name, harness: value.harness,
      about: value.about, repo: value.repo, branch: value.branch, title: value.title, scope: value.scope,
      allow_peer_wake: value.allow_peer_wake, unattended_wakes_remaining: value.unattended_wakes_remaining, last_seen_at: created })) };
  }
  private async send(input: z.infer<typeof sendInputSchema>, meta: Record<string, unknown>) {
    const broker = await this.network(), principal = await this.identity();
    const sender = await this.outbound(meta, principal);
    const canonicalTo = toCanonicalListenerId(input.to);
    const entry = await broker.kv.get(canonicalTo), now = await brokerNow(broker.jsm);
    if (!entry) throw new CitoError("LISTENER_NOT_FOUND", "Target listener does not exist");
    const target = registrationSchema.parse(entry.json());
    const lease = evaluateLease(target.state, now, entry.created);
    if (lease === "UNLISTENED") throw new CitoError("LISTENER_NOT_FOUND", "Target listener was revoked");
    if (lease === "EXPIRED") throw new CitoError("LISTENER_EXPIRED", "Target listener expired");
    if ((sender.caller && canonicalTo === listenerId({ ...sender.endpoint, ...sender.caller }))
      || sender.key === senderEndpointKey(target)) throw new CitoError("SELF_SEND_FORBIDDEN", "Cannot send to your own session");
    if (principal.principal_id !== target.principal_id && target.scope !== "workspace") throw new CitoError("FORBIDDEN_TARGET_SCOPE", "Target is private to its principal");
    const dedup = sha256(JSON.stringify([sender.key, canonicalTo, input.idempotency_key ?? randomUUID()]));
    const hash = sha256(JSON.stringify([input.message, input.wake, input.in_reply_to ?? null]));
    const reservation = this.state.transaction(() => {
      this.state.run("DELETE FROM local_sent_idempotency WHERE expires_at_ms <= ?", Date.now());
      const previous = this.state.db.prepare("SELECT * FROM local_sent_idempotency WHERE dedup_id = ?").get(dedup) as unknown as SentRow | undefined;
      if (previous) {
        if (previous.request_hash !== hash) throw new CitoError("IDEMPOTENCY_KEY_CONFLICT", "Idempotency key was used with another payload");
        return previous;
      }
      const expiry = Date.now() + RETENTION_MS;
      this.state.run(`INSERT INTO local_sent_idempotency (dedup_id, request_hash, signal_id, to_listener_id,
        requested_wake, status, expires_at_iso, expires_at_ms) VALUES (?, ?, ?, ?, ?, 'PENDING_PUBLISH', ?, ?)`,
        dedup, hash, "sig_" + randomUUID().replaceAll("-", ""), canonicalTo, +input.wake, new Date(expiry).toISOString(), expiry);
      return this.state.db.prepare("SELECT * FROM local_sent_idempotency WHERE dedup_id = ?").get(dedup) as unknown as SentRow;
    });
    if (reservation.status !== "PENDING_PUBLISH") return sentResult(reservation, displayId(target));
    let accept!: (value: z.infer<typeof ackSchema> | null) => void;
    const ack = new Promise<z.infer<typeof ackSchema> | null>(resolve => { accept = resolve; });
    const sub = broker.nc.subscribe("cito.ack." + reservation.signal_id, { callback: (error, msg) => {
      if (error) return;
      try {
        const reply = ackSchema.parse(msg.json());
        if (reply.signal_id === reservation.signal_id && reply.status === (reply.effective_wake ? "wake_accepted" : "stored_passive")) accept(reply);
      } catch { /* Ignore malformed replies; timeout preserves the publish milestone. */ }
    } });
    const timer = setTimeout(() => accept(null), 2000);
    const operation = AbortSignal.any([this.shuttingDown.signal, AbortSignal.timeout(10_000)]);
    let interrupt!: () => void;
    const interrupted = new Promise<never>((_, reject) => {
      interrupt = () => reject(new CitoError("NATS_UNAVAILABLE", "Send interrupted or NATS operation deadline exceeded"));
      operation.addEventListener("abort", interrupt, { once: true });
      if (operation.aborted) interrupt();
    });
    try {
      await Promise.race([broker.nc.flush(), interrupted]);
      const published = await Promise.race([broker.js.publish("cito.signal." + canonicalTo, encode({ signal_id: reservation.signal_id,
        to: canonicalTo, target_generation: target.binding_generation, sender_endpoint_key: sender.key,
        from: { principal_id: principal.principal_id, user: principal.owner_user, host_id: this.hostId,
          host_name: this.config.hostName, harness: sender.endpoint.harness,
          listener_id: sender.local?.status === "ACTIVE" ? displayId(sender.local) : null,
          title: sender.caller?.title ?? "", about: sender.local?.about ?? null },
        message: input.message, requested_wake: input.wake, in_reply_to: input.in_reply_to ?? null }), { msgID: dedup }), interrupted]);
      const reply = await Promise.race([ack, interrupted]);
      const held = !input.wake ? "passive_requested" : principal.principal_id !== target.principal_id && !target.allow_peer_wake ? "peer_wake_disabled" : null;
      const effective = reply?.effective_wake ?? held === null;
      const status = reply?.status ?? (effective ? "queued_for_reconnect" : "stored_passive");
      this.state.run("UPDATE local_sent_idempotency SET seq = ?, effective_wake = ?, status = ?, held_reason = ? WHERE dedup_id = ? AND status = 'PENDING_PUBLISH'", published.seq, +effective, status, reply?.held_reason ?? held, dedup);
      return sentResult(this.state.db.prepare("SELECT * FROM local_sent_idempotency WHERE dedup_id = ?").get(dedup) as unknown as SentRow, displayId(target));
    } finally { clearTimeout(timer); sub.unsubscribe(); operation.removeEventListener("abort", interrupt); }
  }
  private async inbox(input: z.infer<typeof inboxInputSchema>, meta: Record<string, unknown>) {
    const principal = await this.identity(true), caller = await this.caller(meta, false, principal);
    const id = listenerId({ ...principal, host_id: this.hostId, harness: caller.harness, session_id: caller.session_id });
    const result = this.state.transaction(() => {
      const listener = this.state.listener(id);
      const rows = this.state.signals(`SELECT * FROM local_signals WHERE listener_id = ? AND acknowledged_at_ms IS NULL
        AND state IN ('RECEIVED','DISPATCHING','DISPATCHED','STORED_PASSIVE') AND expires_at_ms > ? ORDER BY seq LIMIT 50`, id, Date.now());
      const ackIds = new Set([...(input.ack_ids ?? []), ...(input.ack_all ? rows.map(row => row.signal_id) : [])]);
      let count = 0;
      for (const ack of ackIds) count += Number(this.state.run(`UPDATE local_signals SET acknowledged_at_ms = ?,
        state = CASE WHEN state = 'RECEIVED' THEN 'STORED_PASSIVE' ELSE state END WHERE listener_id = ? AND signal_id = ? AND acknowledged_at_ms IS NULL`, Date.now(), id, ack).changes);
      if (input.ack_all) this.state.run("UPDATE local_listeners SET replay_gap_detected = 0, replay_gap_range = NULL WHERE listener_id = ?", id);
      return { ok: true, listener_id: listener ? displayId(listener) : publicListenerId(listenerSlug(caller), id), replay_gap_detected: !!listener?.replay_gap_detected,
        replay_gap_range: listener?.replay_gap_range ?? null, signals: rows.map(row => ({ signal_id: row.signal_id, seq: row.seq,
          from: { user: row.from_user, host_name: row.from_host_name, harness: row.from_harness, listener_id: row.from_listener_id?.startsWith("c_")
            ? publicListenerId(normalizeSlug(row.from_title) || row.from_harness || "listener", row.from_listener_id)
            : row.from_listener_id, about: row.from_about },
          message: row.message, effective_wake: !!row.effective_wake, held_reason: row.held_reason,
          in_reply_to: row.in_reply_to, created_at: row.created_at })), acknowledged_count: count };
    });
    const owner = this.state.listener(id)?.owner_token;
    if (owner) await refreshPiBadge(this.state, this.config, id, owner, this.shuttingDown.signal)
      .catch(error => (this.options.diagnostic ?? console.error)(String(error)));
    return result;
  }
  close(): Promise<void> { return this.closePromise ??= this.shutdown(); }
  private async shutdown(): Promise<void> {
    this.closing = true;
    this.shuttingDown.abort();
    for (const receiver of this.receivers.values()) receiver.cancel();
    // Release broker requests before waiting for their tool calls to settle.
    await this.broker?.nc.close();
    await Promise.allSettled([...this.calls]);
    await Promise.allSettled([...this.receivers.values()].map(async receiver => {
      await receiver.stop();
      await receiver.clearBadge().catch(error => (this.options.diagnostic ?? console.error)(String(error)));
    }));
    this.receivers.clear();
    if (this.brokerPromise) await this.brokerPromise.catch(() => {});
    await this.broker?.nc.close();
    await Promise.allSettled([...this.background]);
    this.state.close();
  }
}
