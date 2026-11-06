import { randomUUID } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";
import { AckPolicy, DeliverPolicy, type Consumer, type ConsumerMessages, type JsMsg, type NatsConnection,
  type QueuedIterator, type KvEntry } from "nats";
import type { BannerPayload } from "../ui/index.js";
import { getAdapter, formatWakeCard, type DeliverContext } from "../adapters/index.js";
import { listenMutex, sessionMutex, brokerNow, localExpirationMs, SIGNALS_STREAM, listenerSlug, toCanonicalListenerId, formatScopedId, sha256,
  type CitoConfig, type provisionNats } from "../shared/index.js";
import { LocalState, registration, registrationSchema, signalSchema, type ListenerRow, type Registration, type SignalRow } from "./state.js";

export type Broker = Awaited<ReturnType<typeof provisionNats>> & { nc: NatsConnection };
export interface ReceiverOptions {
  onBanner?: (payload: BannerPayload) => void;
  onStateChanged?: () => void;
  diagnostic?: (message: string) => void;
  onSessionSwitched?: (harness: string, sessionId: string) => Promise<void>;
  onDirectoryUpsert?: (hub: string, id: string, value: Registration, revision: number) => void;
  onDirectoryRemove?: (hub: string, id: string) => void;
  onReceiverStopped?: (hub: string, id: string, receiver: Receiver) => void;
}
class DispatchStopped extends Error {}
export const encode = (value: unknown) => new TextEncoder().encode(JSON.stringify(value));
const ackPayload = (row: SignalRow) => ({ signal_id: row.signal_id,
  status: row.held_reason ? "stored_passive" : "wake_accepted", held_reason: row.held_reason });

/** One badge aggregates every active hub for the same host conversation. */
export async function refreshSessionBadge(state: LocalState, config: CitoConfig, harness: string, sessionId: string,
  options: { closingKeys?: Set<string>; signal?: AbortSignal } = {}): Promise<void> {
  const hash = sha256(sessionId).slice(0, 16);
  await sessionMutex.run(`${config.stateDir}:${harness}:${hash}`, async () => {
    const rows = state.db.prepare("SELECT * FROM local_listeners WHERE harness = ? AND session_id = ? ORDER BY (status = 'ACTIVE') DESC, binding_generation DESC, hub ASC, listener_id ASC")
      .all(harness, sessionId) as unknown as ListenerRow[];
    const target = rows[0]; if (!target) return;
    const active: ListenerRow[] = [];
    for (const row of rows) {
      if (row.status !== "ACTIVE") continue;
      const key = `${row.hub}:${row.listener_id}`, receiver = state.liveReceivers.get(key);
      if (options.closingKeys?.has(key) || !receiver || receiver.abort.signal.aborted || receiver.owner !== row.owner_token) continue;
      active.push(row);
    }
    const adapter = getAdapter(harness);
    if (!active.length) { await adapter?.updateBadge?.(target, undefined, config, options.signal); return; }
    const unread = active.reduce((sum, row) => sum + Number(state.db.prepare(`SELECT COUNT(*) AS n FROM local_signals
      WHERE hub = ? AND listener_id = ? AND acknowledged_at_ms IS NULL AND expires_at_ms > ? AND state != 'EXPIRED_UNSENT'`)
      .get(row.hub, row.listener_id, Date.now())!.n), 0);
    const slug = target.public_id ? target.public_id.slice(0, -13) : listenerSlug(target);
    const badgeSlug = slug.slice(0, 12).replace(/-+$/g, "") || harness;
    const count = unread > 0 ? ` (${unread})` : "";
    await adapter?.updateBadge?.(target, `🔔 ${badgeSlug}${count}`, config, options.signal);
  });
}

/** Persisted owner/generation and daemon-local critical sections fence effects. */
export class Receiver {
  readonly abort = new AbortController();
  private consumer?: Consumer;
  private messages?: ConsumerMessages;
  private watch?: QueuedIterator<KvEntry>;
  private debounce?: ReturnType<typeof setTimeout>;
  private sweepTimer?: ReturnType<typeof setInterval>;
  private readonly tasks = new Set<Promise<unknown>>();
  private dispatching?: Promise<void>;
  private probing?: Promise<void>;
  private stopped?: Promise<void>;
  private readonly state: LocalState;
  private readonly config: CitoConfig;
  private readonly broker: Broker;
  private readonly options: ReceiverOptions;
  readonly id: string; readonly hub: string; readonly owner: string; readonly generation: number;
  constructor(state: LocalState, config: CitoConfig, broker: Broker,
    row: ListenerRow, options: ReceiverOptions = {}) {
    this.state = state; this.config = config; this.broker = broker; this.options = options;
    this.id = row.listener_id; this.hub = row.hub; this.owner = row.owner_token; this.generation = row.binding_generation;
  }
  private row() { return this.state.listener(this.id, this.hub)!; }
  private log(error: unknown): void { (this.options.diagnostic ?? console.error)(`cito receiver ${this.hub}:${this.id}: ${String(error)}`); }
  private track(task: Promise<unknown>): void {
    this.tasks.add(task);
    void task.catch(error => { if (!this.abort.signal.aborted) this.log(error); }).finally(() => this.tasks.delete(task));
  }
  async start(): Promise<boolean> {
    if (this.abort.signal.aborted) return false;
    try {
      this.state.run("UPDATE local_listeners SET dispatcher_token = NULL, lease_until_ms = 0 WHERE hub = ? AND listener_id = ? AND owner_token = ?", this.hub, this.id, this.owner);
      this.watch = await this.broker.kv.watch({ key: this.id });
      this.track((async () => {
        for await (const entry of this.watch!) {
          if (this.abort.signal.aborted) break;
          let value;
          try { value = entry.operation === "PUT" ? registrationSchema.parse(entry.json()) : null; } catch { value = null; }
          if (!value || value.owner_token !== this.owner || value.binding_generation !== this.generation || value.state !== "ACTIVE") { this.fence(); break; }
        }
      })());
      if (this.abort.signal.aborted) return false;
      const row = this.row();
      const info = await this.broker.jsm.consumers.add(SIGNALS_STREAM, {
        filter_subject: "cito.signal." + this.id, ack_policy: AckPolicy.Explicit,
        deliver_policy: row.persisted_seq ? DeliverPolicy.StartSequence : DeliverPolicy.All,
        ...(row.persisted_seq ? { opt_start_seq: row.persisted_seq + 1 } : {}), max_ack_pending: 50, inactive_threshold: 60_000_000_000,
      });
      this.consumer = await this.broker.js.consumers.get(SIGNALS_STREAM, info.name);
      this.messages = await this.consumer.consume({ max_messages: 50 });
      if (this.abort.signal.aborted) { await this.messages.close(); return false; }
      this.track((async () => {
        for await (const msg of this.messages!) {
          if (this.abort.signal.aborted) break;
          while (!this.abort.signal.aborted) {
            try { await this.receive(msg); break; }
            catch (error) {
              if (this.abort.signal.aborted) break;
              this.log(error); msg.working(); await delay(1000, undefined, { signal: this.abort.signal });
            }
          }
        }
      })());
      this.sweepTimer = setInterval(() => this.track(this.sweep()), 20_000); this.sweepTimer.unref(); this.schedule();
      return true;
    } catch (error) { await this.stop(); throw error; }
  }
  cancel(): void {
    this.abort.abort(); clearTimeout(this.debounce); clearInterval(this.sweepTimer); this.watch?.stop(); void this.messages?.close();
    this.options.onStateChanged?.();
  }
  stop(): Promise<void> {
    return this.stopped ??= (async () => {
      this.cancel(); await Promise.allSettled([...this.tasks, this.dispatching, this.probing]);
      if (!this.broker.nc.isClosed()) await this.consumer?.delete().catch(error => this.log(error));
    })();
  }
  fence(): void {
    this.cancel();
    this.state.transaction(() => {
      this.state.run(`UPDATE local_listeners SET status = 'FENCED_INACTIVE' WHERE hub = ? AND listener_id = ? AND owner_token = ?
        AND binding_generation = ? AND status IN ('ACTIVE','REBINDING')`, this.hub, this.id, this.owner, this.generation);
      this.state.run(`UPDATE local_signals SET state = 'STORED_PASSIVE', held_reason = 'fenced_before_dispatch'
        WHERE hub = ? AND listener_id = ? AND binding_generation <= ? AND state IN ('RECEIVED','DISPATCHING')`, this.hub, this.id, this.generation);
    });
    this.options.onDirectoryRemove?.(this.hub, this.id);
    this.options.onReceiverStopped?.(this.hub, this.id, this);
    this.track(this.badge());
  }
  private async receive(msg: JsMsg): Promise<void> {
    let signal;
    try { signal = signalSchema.parse(msg.json()); } catch (error) { this.log(error); msg.term("invalid signal"); return; }
    if (toCanonicalListenerId(signal.to) !== this.id) { msg.term("wrong recipient"); return; }
    const wall = Date.now(), mono = performance.now(), now = await brokerNow(this.broker.jsm, SIGNALS_STREAM);
    const created = new Date(msg.info.timestampNanos / 1_000_000).toISOString();
    const expires = localExpirationMs(wall, performance.now() - mono, now, created);
    if (this.abort.signal.aborted) return;
    const admitted = this.state.admit(this.id, this.owner, signal, msg.seq, created, expires, this.hub);
    if (admitted.row) this.broker.nc.publish("cito.ack." + signal.signal_id, encode(ackPayload(admitted.row)));
    msg.ack();
    if (!admitted.row || admitted.duplicate || admitted.row.state !== "RECEIVED") return;
    const target = this.row();
    this.options.onBanner?.({ target_harness: target.harness, target_session_id: target.session_id, target_title: target.title,
      from_user: admitted.row.from_user, from_host: admitted.row.from_host_name, from_title: admitted.row.from_title,
      message: admitted.row.message, effective_wake: true });
    this.track(this.badge()); this.schedule();
  }
  badge(): Promise<void> { const row = this.row(); return refreshSessionBadge(this.state, this.config, row.harness, row.session_id); }
  private schedule(delayMs = 1500): void {
    if (this.abort.signal.aborted) return;
    clearTimeout(this.debounce); this.debounce = setTimeout(() => this.track(this.dispatch()), delayMs); this.debounce.unref();
  }
  dispatch(): Promise<void> {
    let completed = false;
    return this.dispatching ??= this.dispatchBatch().then(result => { completed = result; }).finally(() => {
      this.dispatching = undefined;
      if (!this.abort.signal.aborted && this.state.signals("SELECT * FROM local_signals WHERE hub = ? AND listener_id = ? AND state = 'RECEIVED' AND acknowledged_at_ms IS NULL LIMIT 1", this.hub, this.id).length) this.schedule(completed ? 0 : 1500);
    });
  }
  private freeze(): SignalRow[] {
    return this.state.transaction(() => {
      const listener = this.row(), now = Date.now();
      if (this.abort.signal.aborted || listener.status !== "ACTIVE" || listener.owner_token !== this.owner) return [];
      this.state.run(`UPDATE local_signals SET state = 'EXPIRED_UNSENT' WHERE hub = ? AND listener_id = ? AND state = 'DISPATCHING'
        AND batch_expires_at_ms <= ?`, this.hub, this.id, now);
      this.state.run("UPDATE local_signals SET state = 'EXPIRED_UNSENT' WHERE hub = ? AND listener_id = ? AND state = 'RECEIVED' AND expires_at_ms <= ?", this.hub, this.id, now);
      if (listener.dispatcher_token && listener.lease_until_ms > now) return [];
      const token = randomUUID();
      this.state.run("UPDATE local_listeners SET dispatcher_token = ?, lease_until_ms = ? WHERE hub = ? AND listener_id = ?", token, now + 30_000, this.hub, this.id);
      const old = this.state.signals("SELECT * FROM local_signals WHERE hub = ? AND listener_id = ? AND binding_generation = ? AND state = 'DISPATCHING' ORDER BY seq", this.hub, this.id, this.generation)[0];
      const render = (rows: SignalRow[]) => formatWakeCard(listener, rows.map(row => ({ ...row, from_host: row.from_host_name,
        from_listener_id: row.from_listener_id ? formatScopedId(this.hub, row.from_listener_id, this.config) : null })));
      let rows: SignalRow[], batch: string, expires: number, card: string;
      if (old) {
        batch = old.batch_id!; expires = old.batch_expires_at_ms!;
        rows = this.state.signals("SELECT * FROM local_signals WHERE hub = ? AND batch_id = ? ORDER BY seq", this.hub, batch);
        card = old.batch_card ?? render(rows);
      } else {
        const candidates = this.state.signals(`SELECT * FROM local_signals WHERE hub = ? AND listener_id = ? AND binding_generation = ?
          AND state = 'RECEIVED' AND acknowledged_at_ms IS NULL AND expires_at_ms > ? ORDER BY seq LIMIT 50`, this.hub, this.id, this.generation, now);
        rows = []; card = "";
        for (const row of candidates) {
          const next = render([...rows, row]);
          // Include JSON escaping, leaving ample room for the bridge envelope.
          if (Buffer.byteLength(JSON.stringify(next)) > 256 * 1024) {
            if (rows.length) break;
            // An oversized individual card remains readable through the inbox.
            this.state.run("UPDATE local_signals SET state = 'STORED_PASSIVE', held_reason = 'driver_unavailable' WHERE hub = ? AND signal_id = ?", this.hub, row.signal_id);
            continue;
          }
          rows.push(row); card = next;
        }
        if (!rows.length) { this.releaseLease(token); return []; }
        batch = `batch_${this.hub}_${this.id}_${rows[0]!.seq}_${rows.at(-1)!.seq}_${this.generation}`;
        expires = Math.min(...rows.map(row => row.expires_at_ms));
      }
      for (const row of rows) this.state.run(`UPDATE local_signals SET state = 'DISPATCHING', batch_id = ?, batch_expires_at_ms = ?, dispatcher_token = ?, batch_card = ?
        WHERE hub = ? AND signal_id = ?`, batch, expires, token, card, this.hub, row.signal_id);
      return this.state.signals("SELECT * FROM local_signals WHERE hub = ? AND batch_id = ? ORDER BY seq", this.hub, batch);
    });
  }
  private releaseLease(token: string): void {
    this.state.run("UPDATE local_listeners SET dispatcher_token = NULL, lease_until_ms = 0 WHERE hub = ? AND listener_id = ? AND owner_token = ? AND dispatcher_token = ?", this.hub, this.id, this.owner, token);
  }
  private async effect(batch: SignalRow, submit: () => Promise<void>): Promise<void> {
    await listenMutex.run(`${this.hub}:${this.id}`, async () => {
      const entry = await this.broker.kv.get(this.id), remote = entry && registrationSchema.parse(entry.json());
      if (!remote || remote.owner_token !== this.owner || remote.binding_generation !== this.generation || remote.state !== "ACTIVE") this.fence();
      const row = this.row();
      const unread = this.state.signals("SELECT * FROM local_signals WHERE hub = ? AND batch_id = ? AND acknowledged_at_ms IS NULL AND state = 'DISPATCHING' LIMIT 1", this.hub, batch.batch_id!);
      if (this.abort.signal.aborted || row.status !== "ACTIVE" || row.owner_token !== this.owner
        || row.binding_generation !== batch.binding_generation || row.dispatcher_token !== batch.dispatcher_token
        || Date.now() >= batch.batch_expires_at_ms! || !unread.length) throw new DispatchStopped();
      this.state.run("UPDATE local_listeners SET lease_until_ms = ? WHERE hub = ? AND listener_id = ? AND owner_token = ?", Date.now() + 30_000, this.hub, this.id, this.owner);
      await submit();
    });
  }
  private async runEffect(batch: SignalRow, submit: () => Promise<void>, opts: Parameters<DeliverContext["runEffect"]>[1]): Promise<boolean> {
    for (let attempt = 0; attempt < opts.attempts; attempt++) {
      try { await this.effect(batch, submit); return true; }
      catch (error) {
        if (error instanceof DispatchStopped || this.abort.signal.aborted) throw new DispatchStopped();
        const failure = error as { killed?: boolean; signal?: string; code?: unknown };
        if (opts.onSigtermTimeout && failure.killed && failure.signal === "SIGTERM" && typeof failure.code !== "number") { opts.onSigtermTimeout(); return true; }
        if (attempt + 1 < opts.attempts) await delay(opts.pause[attempt]!, undefined, { signal: this.abort.signal });
      }
    }
    return false;
  }
  private async dispatchBatch(): Promise<boolean> {
    const rows = this.freeze(); if (!rows.length) return false;
    const first = rows[0]!, token = first.dispatcher_token!, batch = first.batch_id!;
    try {
      const listener = this.row();
      const outcome = await getAdapter(listener.harness)!.deliver(listener, {
        batchId: batch, card: first.batch_card!, queueWritten: rows.every(row => !!row.queue_written),
        markQueueWritten: () => { this.state.run("UPDATE local_signals SET queue_written = 1 WHERE hub = ? AND batch_id = ? AND state = 'DISPATCHING' AND dispatcher_token = ?", this.hub, batch, token); },
        runEffect: (submit, opts) => this.runEffect(first, submit, opts), config: this.config, signal: this.abort.signal,
      });
      if (outcome === "session_switched") {
        if (this.options.onSessionSwitched) await this.options.onSessionSwitched(listener.harness, listener.session_id);
        else await this.revoke("session_switched");
      } else if (outcome === "driver_unavailable") {
        this.state.run("UPDATE local_signals SET state = 'STORED_PASSIVE', held_reason = 'driver_unavailable' WHERE hub = ? AND batch_id = ? AND state = 'DISPATCHING' AND dispatcher_token = ?", this.hub, batch, token);
      } else {
        this.state.run(`UPDATE local_signals SET state = 'DISPATCHED', acknowledged_at_ms = CASE WHEN ? THEN COALESCE(acknowledged_at_ms, ?) ELSE acknowledged_at_ms END
          WHERE hub = ? AND batch_id = ? AND state = 'DISPATCHING' AND dispatcher_token = ?`, +(outcome === "delivered"), Date.now(), this.hub, batch, token);
      }
      await this.badge();
    } catch (error) {
      if (this.abort.signal.aborted) return true;
      if (!(error instanceof DispatchStopped)) this.log(error);
      this.state.run(`UPDATE local_signals SET state = CASE WHEN batch_expires_at_ms <= ? THEN 'EXPIRED_UNSENT' ELSE 'STORED_PASSIVE' END
        WHERE hub = ? AND batch_id = ? AND state = 'DISPATCHING' AND dispatcher_token = ?`, Date.now(), this.hub, batch, token);
    } finally { this.releaseLease(token); }
    return true;
  }
  async revoke(reason: string): Promise<void> {
    this.cancel();
    await listenMutex.run(`${this.hub}:${this.id}`, async () => {
      if (this.row()?.owner_token !== this.owner) return;
      this.state.transaction(() => {
        this.state.run("UPDATE local_listeners SET status = 'UNLISTENED' WHERE hub = ? AND listener_id = ? AND owner_token = ?", this.hub, this.id, this.owner);
        this.state.run(`UPDATE local_signals SET state = 'STORED_PASSIVE', held_reason = ? WHERE hub = ? AND listener_id = ?
          AND binding_generation <= ? AND state IN ('RECEIVED','DISPATCHING')`, reason, this.hub, this.id, this.generation);
      });
      this.options.onDirectoryRemove?.(this.hub, this.id);
      this.options.onReceiverStopped?.(this.hub, this.id, this);
      await this.badge().catch(error => this.log(error));
      const entry = await this.broker.kv.get(this.id);
      if (entry) {
        const value = registrationSchema.parse(entry.json());
        if (value.owner_token === this.owner && value.binding_generation === this.generation) await this.broker.kv.update(this.id,
          encode({ ...value, state: "UNLISTENED", binding_generation: this.generation + 1 }), entry.revision);
      }
    });
  }
  sweep(): Promise<void> { return this.probing ??= this.probe().finally(() => { this.probing = undefined; }); }
  private async probe(): Promise<void> {
    if (this.abort.signal.aborted) return;
    const initial = this.row();
    if (initial.owner_token !== this.owner || initial.status !== "ACTIVE") { this.fence(); return; }
    const outcome = await getAdapter(initial.harness)!.probe(initial, this.config, AbortSignal.any([this.abort.signal, AbortSignal.timeout(1500)]));
    if (this.abort.signal.aborted) return;
    if (outcome.status === "switched") {
      if (this.options.onSessionSwitched) await this.options.onSessionSwitched(initial.harness, initial.session_id);
      else await this.revoke("session_switched");
      return;
    }
    if (outcome.status !== "alive") {
      this.state.run("UPDATE local_listeners SET consecutive_probe_failures = consecutive_probe_failures + 1 WHERE hub = ? AND listener_id = ? AND owner_token = ?", this.hub, this.id, this.owner);
      if (outcome.status === "closed_definitive" || this.row().consecutive_probe_failures >= 3) await this.revoke("session_closed");
      return;
    }
    await listenMutex.run(`${this.hub}:${this.id}`, async () => {
      const row = this.row(); if (this.abort.signal.aborted || row.owner_token !== this.owner || row.status !== "ACTIVE") return;
      this.state.run("UPDATE local_listeners SET consecutive_probe_failures = 0 WHERE hub = ? AND listener_id = ? AND owner_token = ?", this.hub, this.id, this.owner);
      const entry = await this.broker.kv.get(this.id), value = entry && registrationSchema.parse(entry.json());
      if (!value || value.owner_token !== this.owner || value.binding_generation !== this.generation || value.state !== "ACTIVE") { this.options.onDirectoryRemove?.(this.hub, this.id); this.fence(); return; }
      if (row.scope !== value.scope) { this.state.run("UPDATE local_listeners SET scope = ? WHERE hub = ? AND listener_id = ?", value.scope, this.hub, this.id); this.options.onStateChanged?.(); }
      const nextReg = registration(this.row());
      const rev = await this.broker.kv.update(this.id, encode(nextReg), entry!.revision);
      this.options.onDirectoryUpsert?.(this.hub, this.id, nextReg, rev);
    });
  }
}
