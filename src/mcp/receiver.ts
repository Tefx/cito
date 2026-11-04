import { randomUUID } from "node:crypto";
import { uptime } from "node:os";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { AckPolicy, DeliverPolicy, type Consumer, type ConsumerMessages, type JsMsg, type NatsConnection,
  type QueuedIterator, type KvEntry } from "nats";
import { acquireFlock, showBanner, type BannerOptions } from "../banner/index.js";
import { assertCodexThreadOpen, readCodexThread, identifyPi, setPiStatus, deliverPiWake, PiBridgeError,
  queueCodexWake, triggerCodexWake, formatWakeCard, CodexSessionError, codexDesktopRunning, type CodexDriverOptions, type PiIdentity } from "../adapters/index.js";
import { brokerNow, localExpirationMs, SIGNALS_STREAM, type CitoConfig, type provisionNats } from "../shared/index.js";
import { LocalState, registration, registrationSchema, signalSchema, type ListenerRow, type SignalRow } from "./state.js";

export type Broker = Awaited<ReturnType<typeof provisionNats>> & { nc: NatsConnection };
export interface ReceiverOptions { banner?: BannerOptions; codex?: Pick<CodexDriverOptions, "openBin">; diagnostic?: (message: string) => void }
class DispatchStopped extends Error {}
export const encode = (value: unknown) => new TextEncoder().encode(JSON.stringify(value));
export const listenLock = (config: CitoConfig, id: string) => join(config.stateDir, "sockets", `listen-${id}.lock`);
const ackPayload = (row: SignalRow) => ({ signal_id: row.signal_id, effective_wake: !!row.effective_wake,
  status: row.effective_wake ? "wake_accepted" : "stored_passive", held_reason: row.held_reason });

/** One process-owned receiver; SQLite owner_token guards every write and every host effect. */
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
  private ownerLock?: NonNullable<Awaited<ReturnType<typeof acquireFlock>>>;
  readonly id: string;
  readonly owner: string;
  readonly generation: number;
  constructor(private readonly state: LocalState, private readonly config: CitoConfig, private readonly broker: Broker,
    row: ListenerRow, private readonly options: ReceiverOptions = {}) {
    this.id = row.listener_id; this.owner = row.owner_token; this.generation = row.binding_generation;
  }
  private log(error: unknown): void { (this.options.diagnostic ?? console.error)(`cito receiver ${this.id}: ${error instanceof Error ? error.message : String(error)}`); }
  private track(task: Promise<unknown>): void {
    this.tasks.add(task);
    void task.catch(error => { if (!this.abort.signal.aborted) this.log(error); }).finally(() => this.tasks.delete(task));
  }
  async start(ownerWaitMs = 5000): Promise<boolean> {
    if (this.abort.signal.aborted) return false;
    // A persisted owner may be discovered by several MCP processes. Only the
    // live holder may consume/dispatch; kernel flock releases on process exit.
    const ownerLock = await acquireFlock(join(this.config.stateDir, "sockets", `receiver-${this.id}.lock`), ownerWaitMs);
    if (!ownerLock) return false;
    if (this.abort.signal.aborted) { await ownerLock.release(); return false; }
    this.ownerLock = ownerLock;
    try {
      this.state.run("UPDATE local_listeners SET dispatcher_token = NULL, lease_until_ms = 0 WHERE listener_id = ? AND owner_token = ?", this.id, this.owner);
      this.watch = await this.broker.kv.watch({ key: this.id });
      this.track((async () => {
        for await (const entry of this.watch!) {
          if (this.abort.signal.aborted) break;
          const value = entry.operation === "PUT" ? registrationSchema.parse(entry.json()) : null;
          if (!value || value.owner_token !== this.owner || value.binding_generation !== this.generation || value.state !== "ACTIVE") {
            this.fence(); break;
          }
        }
      })());
      if (this.abort.signal.aborted) return false;
      const row = this.state.listener(this.id)!;
      const info = await this.broker.jsm.consumers.add(SIGNALS_STREAM, {
        filter_subject: "cito.signal." + this.id, ack_policy: AckPolicy.Explicit,
        deliver_policy: row.persisted_seq ? DeliverPolicy.StartSequence : DeliverPolicy.All,
        ...(row.persisted_seq ? { opt_start_seq: row.persisted_seq + 1 } : {}),
        max_ack_pending: 50, inactive_threshold: 60_000_000_000,
      });
      this.consumer = await this.broker.js.consumers.get(SIGNALS_STREAM, info.name);
      this.messages = await this.consumer.consume({ max_messages: 50 });
      if (this.abort.signal.aborted) { await this.messages.close(); return false; }
      this.track((async () => {
        for await (const msg of this.messages!) {
          if (this.abort.signal.aborted) break;
          // Never advance persisted_seq past an earlier failed admission. A later
          // commit would otherwise make a restart skip the uncommitted message.
          while (!this.abort.signal.aborted) {
            try { await this.receive(msg); break; }
            catch (error) {
              if (this.abort.signal.aborted) break;
              this.log(error); msg.working();
              await delay(1000, undefined, { signal: this.abort.signal });
            }
          }
        }
      })());
      this.sweepTimer = setInterval(() => { this.track(this.sweep()); }, 20_000);
      this.sweepTimer.unref();
      this.schedule();
      return true;
    } catch (error) { await this.stop(); throw error; }
  }
  cancel(): void {
    this.abort.abort();
    clearTimeout(this.debounce); clearInterval(this.sweepTimer);
    this.watch?.stop();
    void this.messages?.close();
    const lock = this.ownerLock; this.ownerLock = undefined;
    if (lock) this.track(lock.release());
  }
  stop(): Promise<void> {
    return this.stopped ??= (async () => {
      this.cancel();
      await Promise.allSettled([...this.tasks, this.dispatching, this.probing]);
      if (!this.broker.nc.isClosed()) await this.consumer?.delete().catch(error => this.log(error));
    })();
  }
  fence(): void {
    this.cancel();
    this.state.transaction(() => {
      this.state.run(`UPDATE local_listeners SET status = 'FENCED_INACTIVE' WHERE listener_id = ? AND owner_token = ?
        AND binding_generation = ? AND status IN ('ACTIVE','REBINDING')`, this.id, this.owner, this.generation);
      this.state.run(`UPDATE local_signals SET state = 'STORED_PASSIVE', held_reason = 'fenced_before_dispatch'
        WHERE listener_id = ? AND binding_generation <= ? AND state IN ('RECEIVED','DISPATCHING')`, this.id, this.generation);
    });
  }
  private async receive(msg: JsMsg): Promise<void> {
    let signal;
    try { signal = signalSchema.parse(msg.json()); }
    catch (error) { this.log(error); msg.term("invalid signal"); return; }
    if (signal.to !== this.id) { msg.term("wrong recipient"); return; }
    const wall = Date.now(), mono = performance.now();
    const now = await brokerNow(this.broker.jsm, SIGNALS_STREAM);
    const created = new Date(msg.info.timestampNanos / 1_000_000).toISOString();
    const expires = localExpirationMs(wall, performance.now() - mono, now, created);
    if (this.abort.signal.aborted) return;
    const admitted = this.state.admit(this.id, this.owner, signal, msg.seq, created, expires, Math.floor(uptime() * 1000));
    if (admitted.row) this.broker.nc.publish("cito.ack." + signal.signal_id, encode(ackPayload(admitted.row)));
    msg.ack();
    if (!admitted.row || admitted.duplicate) return;
    this.track(this.banner(admitted.row));
    if (admitted.row.effective_wake) this.schedule();
    else this.track(this.badge());
  }
  private async banner(row: SignalRow): Promise<void> {
    const target = this.state.listener(this.id)!;
    await showBanner({ target_harness: target.harness, target_session_id: target.session_id, target_title: target.title,
      from_user: row.from_user, from_host: row.from_host_name, from_title: row.from_title,
      message: row.message, effective_wake: !!row.effective_wake }, { ...this.options.banner, config: this.config });
  }
  async badge(): Promise<void> {
    const row = this.state.listener(this.id);
    if (!row || row.owner_token !== this.owner || row.harness !== "pi" || !row.pi_pid || row.status !== "ACTIVE") return;
    const unread = Number(this.state.db.prepare(`SELECT COUNT(*) AS n FROM local_signals
      WHERE listener_id = ? AND acknowledged_at_ms IS NULL AND expires_at_ms > ?`).get(this.id, Date.now())!.n);
    await setPiStatus({ pi_pid: row.pi_pid, session_id: row.session_id }, `📡 cito: ${this.id}${unread ? ` (${unread} unread)` : ""}`, this.config, this.abort.signal);
  }
  private schedule(): void {
    if (this.abort.signal.aborted) return;
    clearTimeout(this.debounce);
    this.debounce = setTimeout(() => this.track(this.dispatch()), 1500);
    this.debounce.unref();
  }
  dispatch(): Promise<void> {
    if (this.dispatching) return this.dispatching;
    this.dispatching = this.dispatchBatch().finally(() => {
      this.dispatching = undefined;
      if (!this.abort.signal.aborted && this.state.signals("SELECT * FROM local_signals WHERE listener_id = ? AND state = 'RECEIVED' LIMIT 1", this.id).length) this.schedule();
    });
    return this.dispatching;
  }
  private freeze(): SignalRow[] {
    return this.state.transaction(() => {
      const listener = this.state.listener(this.id)!;
      const now = Date.now();
      if (this.abort.signal.aborted || listener.status !== "ACTIVE" || listener.owner_token !== this.owner) return [];
      // Expire a frozen batch as a unit BEFORE individual-row expiry can hide its earliest member.
      this.state.run(`UPDATE local_signals SET state = 'EXPIRED_UNSENT' WHERE listener_id = ? AND state = 'DISPATCHING'
        AND batch_id IN (SELECT batch_id FROM local_signals WHERE listener_id = ? AND batch_expires_at_ms <= ?)`, this.id, this.id, now);
      this.state.run("UPDATE local_signals SET state = 'EXPIRED_UNSENT' WHERE listener_id = ? AND state = 'RECEIVED' AND expires_at_ms <= ?", this.id, now);
      if (listener.dispatcher_token && listener.lease_until_ms > now) return [];
      const token = randomUUID();
      this.state.run("UPDATE local_listeners SET dispatcher_token = ?, lease_until_ms = ? WHERE listener_id = ?", token, now + 30_000, this.id);
      const old = this.state.signals(`SELECT * FROM local_signals WHERE listener_id = ? AND binding_generation = ? AND state = 'DISPATCHING' ORDER BY seq`, this.id, this.generation)[0];
      let rows: SignalRow[], batch: string, expires: number;
      if (old) {
        batch = old.batch_id!; expires = old.batch_expires_at_ms!;
        rows = this.state.signals("SELECT * FROM local_signals WHERE batch_id = ? ORDER BY seq", batch);
      } else {
        rows = this.state.signals(`SELECT * FROM local_signals WHERE listener_id = ? AND binding_generation = ?
          AND state = 'RECEIVED' AND acknowledged_at_ms IS NULL AND expires_at_ms > ? ORDER BY seq`, this.id, this.generation, now);
        if (!rows.length) { this.releaseLease(token); return []; }
        batch = `batch_${this.id}_${rows[0]!.seq}_${rows.at(-1)!.seq}_${this.generation}`;
        expires = Math.min(...rows.map(row => row.expires_at_ms));
      }
      const card = old?.batch_card ?? formatWakeCard(listener, rows.map(row => ({ ...row, from_host: row.from_host_name })));
      for (const row of rows) this.state.run(`UPDATE local_signals SET state = 'DISPATCHING', batch_id = ?, batch_expires_at_ms = ?, dispatcher_token = ?, batch_card = ? WHERE signal_id = ?`, batch, expires, token, card, row.signal_id);
      return this.state.signals("SELECT * FROM local_signals WHERE batch_id = ? ORDER BY seq", batch);
    });
  }
  private releaseLease(token: string): void {
    this.state.run("UPDATE local_listeners SET dispatcher_token = NULL, lease_until_ms = 0 WHERE listener_id = ? AND owner_token = ? AND dispatcher_token = ?", this.id, this.owner, token);
  }
  private async effect(batch: SignalRow, submit: () => Promise<void>): Promise<void> {
    const lock = await acquireFlock(listenLock(this.config, this.id), 5000);
    if (!lock) throw new Error("Listener dispatch lock timeout");
    try {
      const row = this.state.listener(this.id)!;
      const entry = await this.broker.kv.get(this.id);
      const remote = entry && registrationSchema.parse(entry.json());
      if (!remote || remote.owner_token !== this.owner || remote.binding_generation !== this.generation || remote.state !== "ACTIVE") this.fence();
      const unread = this.state.signals("SELECT * FROM local_signals WHERE batch_id = ? AND acknowledged_at_ms IS NULL AND state = 'DISPATCHING' LIMIT 1", batch.batch_id!);
      if (this.abort.signal.aborted || row.status !== "ACTIVE" || row.owner_token !== this.owner
        || row.binding_generation !== batch.binding_generation || row.dispatcher_token !== batch.dispatcher_token
        || Date.now() >= batch.batch_expires_at_ms! || !unread.length) throw new DispatchStopped();
      this.state.run("UPDATE local_listeners SET lease_until_ms = ? WHERE listener_id = ? AND owner_token = ?", Date.now() + 30_000, this.id, this.owner);
      await submit();
    } finally { await lock.release(); }
  }
  private async retry(batch: SignalRow, attempts: number, pause: number[], submit: () => Promise<void>, queue = false): Promise<boolean> {
    for (let attempt = 0; attempt < attempts; attempt++) {
      try { await this.effect(batch, submit); return true; }
      catch (error) {
        if (error instanceof DispatchStopped || this.abort.signal.aborted) throw new DispatchStopped();
        if (error instanceof PiBridgeError && error.bridgeError === "SESSION_MISMATCH") {
          await this.revoke("session_switched"); throw new DispatchStopped();
        }
        // execFile reports timeout as killed+SIGTERM, with no numeric exit code.
        const failure = error as { killed?: boolean; signal?: string; code?: unknown };
        if (queue && failure.killed && failure.signal === "SIGTERM" && typeof failure.code !== "number") return true;
        if (attempt + 1 < attempts) await delay(pause[attempt]!, undefined, { signal: this.abort.signal });
      }
    }
    return false;
  }
  private async dispatchBatch(): Promise<void> {
    const rows = this.freeze(); if (!rows.length) return;
    const first = rows[0]!, token = first.dispatcher_token!, batch = first.batch_id!;
    const listener = this.state.listener(this.id)!;
    const card = first.batch_card!;
    const passive = () => this.state.run(`UPDATE local_signals SET state = 'STORED_PASSIVE', held_reason = 'driver_unavailable'
      WHERE batch_id = ? AND state = 'DISPATCHING' AND dispatcher_token = ?`, batch, token);
    try {
      if (listener.harness === "pi") {
        if (!await this.retry(first, 3, [1000, 2000], async () => {
          await deliverPiWake({ pi_pid: listener.pi_pid!, session_id: listener.session_id }, batch, card, this.config, this.abort.signal);
        })) { passive(); return; }
      } else {
        if (!rows.every(row => row.queue_written)) {
          if (!await this.retry(first, 3, [1000, 2000], () => queueCodexWake(listener.session_id, card,
            { codexBin: this.config.codexBin, signal: this.abort.signal }), true)) { passive(); return; }
          this.state.run("UPDATE local_signals SET queue_written = 1 WHERE batch_id = ? AND state = 'DISPATCHING' AND dispatcher_token = ?", batch, token);
        }
        await this.retry(first, 2, [500], () => triggerCodexWake(listener.session_id,
          { ...this.options.codex, signal: this.abort.signal }));
      }
      this.state.run("UPDATE local_signals SET state = 'DISPATCHED' WHERE batch_id = ? AND state = 'DISPATCHING' AND dispatcher_token = ?", batch, token);
    } catch (error) {
      // Revocation/fencing owns its reason; shutdown leaves the frozen batch for recovery.
      if (this.abort.signal.aborted) return;
      if (!(error instanceof DispatchStopped)) this.log(error);
      // Acknowledged or expired batches must never be retried into a host effect.
      this.state.run(`UPDATE local_signals SET state = CASE WHEN batch_expires_at_ms <= ? THEN 'EXPIRED_UNSENT' ELSE 'STORED_PASSIVE' END
        WHERE batch_id = ? AND state = 'DISPATCHING' AND dispatcher_token = ?`, Date.now(), batch, token);
    } finally { this.releaseLease(token); }
  }
  async revoke(reason: string): Promise<void> {
    this.cancel();
    this.state.transaction(() => {
      this.state.run("UPDATE local_listeners SET status = 'UNLISTENED' WHERE listener_id = ? AND owner_token = ?", this.id, this.owner);
      this.state.run(`UPDATE local_signals SET state = 'STORED_PASSIVE', held_reason = ? WHERE listener_id = ?
        AND binding_generation <= ? AND state IN ('RECEIVED','DISPATCHING')`, reason, this.id, this.generation);
    });
    const entry = await this.broker.kv.get(this.id);
    if (entry) {
      const value = registrationSchema.parse(entry.json());
      if (value.owner_token === this.owner && value.binding_generation === this.generation) {
        await this.broker.kv.update(this.id, encode({ ...value, state: "UNLISTENED", binding_generation: this.generation + 1 }), entry.revision);
      }
    }
  }
  sweep(): Promise<void> {
    if (this.probing) return this.probing;
    this.probing = this.probe().finally(() => { this.probing = undefined; });
    return this.probing;
  }
  private async probe(): Promise<void> {
    if (this.abort.signal.aborted) return;
    const initial = this.state.listener(this.id)!;
    if (initial.owner_token !== this.owner || initial.status !== "ACTIVE") { this.fence(); return; }
    let pi: PiIdentity | undefined;
    const probeSignal = AbortSignal.any([this.abort.signal, AbortSignal.timeout(1500)]);
    try {
      if (initial.harness === "pi") {
        pi = await identifyPi(initial.pi_pid!, this.config, probeSignal);
        if (pi.session_id !== initial.session_id) { await this.revoke("session_switched"); return; }
      } else {
        readCodexThread(initial.session_id, this.config);
        await assertCodexThreadOpen(initial.session_id, this.config, probeSignal);
      }
    } catch (error) {
      if (this.abort.signal.aborted) return;
      let dead = error instanceof PiBridgeError && ["SESSION_MISMATCH", "SESSION_NOT_OPEN"].includes(error.bridgeError);
      if (initial.harness === "pi" && error instanceof Error && ["ENOENT", "ECONNREFUSED"].includes((error.cause as NodeJS.ErrnoException | undefined)?.code ?? "")) {
        try { process.kill(initial.pi_pid!, 0); } catch (cause) { if ((cause as NodeJS.ErrnoException).code === "ESRCH") dead = true; }
      }
      if (initial.harness === "codex") {
        dead = error instanceof CodexSessionError && error.definitive;
        if (!dead && !probeSignal.aborted && error instanceof Error
          && ["ENOENT", "ECONNREFUSED"].includes((error.cause as NodeJS.ErrnoException | undefined)?.code ?? "")) {
          try { dead = !await codexDesktopRunning(probeSignal); } catch { /* A failed process probe remains transient. */ }
        }
      }
      this.state.run("UPDATE local_listeners SET consecutive_probe_failures = consecutive_probe_failures + 1 WHERE listener_id = ? AND owner_token = ?", this.id, this.owner);
      if (dead || this.state.listener(this.id)!.consecutive_probe_failures >= 3) await this.revoke("session_closed");
      return;
    }
    const lock = await acquireFlock(listenLock(this.config, this.id), 5000);
    if (!lock) return;
    try {
      const row = this.state.listener(this.id)!;
      if (this.abort.signal.aborted || row.owner_token !== this.owner || row.status !== "ACTIVE") return;
      this.state.transaction(() => {
        if (pi) this.state.observePi(this.id, pi.bridge_epoch, pi.human_event_count);
        this.state.consumeHuman(this.id);
        this.state.run("UPDATE local_listeners SET consecutive_probe_failures = 0 WHERE listener_id = ? AND owner_token = ?", this.id, this.owner);
      });
      const entry = await this.broker.kv.get(this.id);
      if (!entry) { this.fence(); return; }
      const value = registrationSchema.parse(entry.json());
      if (value.owner_token !== this.owner || value.binding_generation !== this.generation || value.state !== "ACTIVE") { this.fence(); return; }
      await this.broker.kv.update(this.id, encode(registration(this.state.listener(this.id)!)), entry.revision);
    } finally { await lock.release(); }
  }
}
