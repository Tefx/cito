import { EventEmitter } from "node:events";
import { Agent as HttpAgent, request as httpRequest } from "node:http";
import { Agent as HttpsAgent, request as httpsRequest } from "node:https";
import { StringDecoder } from "node:string_decoder";
import { CitoError } from "../shared/errors.js";
import { localPrincipal, type Principal } from "../shared/identity.js";
import {
  DEFAULT_KV_STREAM_CONFIG,
  DEFAULT_SIGNALS_STREAM_CONFIG,
  HubEngine,
  HubError,
  KV_LISTENERS_STREAM,
  LISTENERS_BUCKET,
  SIGNALS_STREAM,
  isAllowedCitoSubject,
  subjectMatchesFilter,
  type HubKvEvent,
  type HubNcEvent,
  type HubSignalEvent,
  type StreamConfig,
} from "./engine.js";

export { LISTENERS_BUCKET, KV_LISTENERS_STREAM, SIGNALS_STREAM, type StreamConfig };

export const Events = {
  Disconnect: "disconnect",
  Reconnect: "reconnect",
  Error: "error",
} as const;

export const ErrorCode = {
  AuthorizationViolation: "AUTHORIZATION_VIOLATION",
  AuthenticationExpired: "AUTHENTICATION_EXPIRED",
  PermissionsViolation: "PERMISSIONS_VIOLATION",
} as const;
export type ErrorCode = (typeof ErrorCode)[keyof typeof ErrorCode];

export const AckPolicy = {
  Explicit: "explicit",
  None: "none",
  All: "all",
} as const;

export const DeliverPolicy = {
  All: "all",
  StartSequence: "by_start_sequence",
} as const;

export const RetentionPolicy = {
  Limits: "limits",
} as const;

export const StorageType = {
  File: "file",
  Memory: "memory",
} as const;

export const DiscardPolicy = {
  Old: "old",
  New: "new",
} as const;

const encoder = new TextEncoder();
const decoder = new TextDecoder();

export function StringCodec() {
  return {
    encode: (str: string): Uint8Array => encoder.encode(str),
    decode: (bytes: Uint8Array): string => decoder.decode(bytes),
  };
}

export class SimpleQueuedIterator<T> {
  private readonly queue: T[] = [];
  private readonly waiters: { resolve: (res: IteratorResult<T>) => void; reject: (err: unknown) => void }[] = [];
  private stopped = false;
  private stopError?: Error;
  private onStop?: () => void;

  constructor(onStop?: () => void) {
    this.onStop = onStop;
  }

  push(item: T): void {
    if (this.stopped) return;
    const waiter = this.waiters.shift();
    if (waiter) {
      waiter.resolve({ value: item, done: false });
    } else {
      this.queue.push(item);
    }
  }

  stop(err?: Error): void {
    if (this.stopped) return;
    this.stopped = true;
    this.stopError = err;
    this.onStop?.();
    this.onStop = undefined;
    if (err) {
      while (this.waiters.length > 0) {
        this.waiters.shift()!.reject(err);
      }
    } else if (this.queue.length === 0) {
      while (this.waiters.length > 0) {
        this.waiters.shift()!.resolve({ value: undefined as never, done: true });
      }
    }
  }

  getPending(): number {
    return this.queue.length;
  }

  [Symbol.asyncIterator](): AsyncIterator<T> {
    return {
      next: (): Promise<IteratorResult<T>> => {
        if (this.stopError) {
          const err = this.stopError;
          this.stopError = undefined;
          return Promise.reject(err);
        }
        if (this.queue.length > 0) {
          const item = this.queue.shift()!;
          if (this.stopped && this.queue.length === 0) {
            while (this.waiters.length > 0) {
              this.waiters.shift()!.resolve({ value: undefined as never, done: true });
            }
          }
          return Promise.resolve({ value: item, done: false });
        }
        if (this.stopped) {
          return Promise.resolve({ value: undefined as never, done: true });
        }
        return new Promise<IteratorResult<T>>((resolve, reject) => {
          this.waiters.push({ resolve, reject });
        });
      },
      return: (): Promise<IteratorResult<T>> => {
        this.stop();
        return Promise.resolve({ value: undefined as never, done: true });
      },
    };
  }
}

export type QueuedIterator<T> = SimpleQueuedIterator<T>;

export interface KvEntry {
  bucket: string;
  key: string;
  value: Uint8Array;
  created: Date;
  revision: number;
  delta: number;
  operation: "PUT" | "DEL" | "PURGE";
  json<T = unknown>(): T;
  string(): string;
}

export interface JsMsg {
  seq: number;
  subject: string;
  data: Uint8Array;
  info: {
    timestampNanos: number;
    stream: string;
    consumer: string;
    ackedAtMs?: number | null;
  };
  json<T = unknown>(): T;
  string(): string;
  ack(): void;
  ackAck(): Promise<boolean>;
  term(reason?: string): void;
  working(): void;
}

export interface ConsumerMessages extends AsyncIterable<JsMsg> {
  close(): Promise<void>;
  stop(): void;
}

export interface Consumer {
  consume(opts?: { max_messages?: number }): Promise<ConsumerMessages>;
  next(opts?: { expires?: number }): Promise<JsMsg | null>;
  delete(): Promise<boolean>;
}

export interface KvView {
  get(key: string, opts?: { checkForbiddenScope?: boolean }): Promise<KvEntry | null>;
  create(key: string, data: Uint8Array): Promise<number>;
  update(key: string, data: Uint8Array, expectedRevision: number): Promise<number>;
  put(key: string, data: Uint8Array): Promise<number>;
  delete(key: string): Promise<void>;
  purge(key: string): Promise<void>;
  keys(): Promise<QueuedIterator<string>>;
  watch(opts?: {
    key?: string;
    ignoreDeletes?: boolean;
    initializedFn?: () => void;
  }): Promise<QueuedIterator<KvEntry>>;
}

export interface JetStreamClient {
  publish(
    subject: string,
    data?: Uint8Array,
    opts?: { msgID?: string },
  ): Promise<{ seq: number; duplicate: boolean; stream: string }>;
  views: {
    kv(bucket: string, opts?: Record<string, unknown>): Promise<KvView>;
  };
  consumers: {
    get(stream: string, name: string): Promise<Consumer>;
  };
}

export interface JetStreamManager {
  getAccountInfo(): Promise<{ streams: number; consumers: number; memory: number; storage: number }>;
  streams: {
    info(stream: string): Promise<{
      config: StreamConfig;
      state: { messages: number; last_seq: number; first_seq: number; bytes: number; consumer_count: number };
      ts: string;
    }>;
    add(config: Partial<StreamConfig> & { name: string }): Promise<unknown>;
    update(stream: string, patch: Partial<StreamConfig>): Promise<unknown>;
    getMessage(
      stream: string,
      query: { seq: number },
    ): Promise<{
      seq: number;
      subject: string;
      data: Uint8Array;
      time: Date;
      json<T = unknown>(): T;
      string(): string;
    }>;
  };
  consumers: {
    add(stream: string, config: Record<string, unknown>): Promise<{ name: string; stream_name: string; config: Record<string, unknown> }>;
    delete(stream: string, name: string): Promise<boolean>;
  };
}

export interface Msg {
  subject: string;
  data: Uint8Array;
  headers?: { get(key: string): string | undefined };
  json<T = unknown>(): T;
  string(): string;
  respond(data?: Uint8Array): boolean;
}

export interface Subscription {
  unsubscribe(): void;
}

export interface NatsConnection {
  rpc<T = unknown>(op: string, params?: Record<string, unknown>): Promise<T>;
  publish(subject: string, data?: Uint8Array): void;
  subscribe(
    subject: string,
    opts?: { queue?: string; callback?: (error: Error | null, msg: Msg) => void },
  ): Subscription;
  flush(): Promise<void>;
  close(): Promise<void>;
  drain(): Promise<void>;
  isClosed(): boolean;
  closed(): Promise<void | Error>;
  status(): QueuedIterator<{ type: string; data?: unknown }>;
  reconnect(): Promise<void>;
  jetstream(): JetStreamClient;
  jetstreamManager(): Promise<JetStreamManager>;
}

export interface ConnectionOptions {
  servers?: string | string[];
  token?: string;
  user?: string;
  pass?: string;
  name?: string;
  timeout?: number;
  maxReconnectAttempts?: number;
  stateDir?: string;
}

export interface Broker {
  nc: NatsConnection;
  js: JetStreamClient;
  jsm: JetStreamManager;
  kv: KvView;
}

const localEngines = new Map<string, HubEngine>();

export function getLocalHubEngine(stateDir = "__default__"): HubEngine {
  let engine = localEngines.get(stateDir);
  if (!engine) {
    engine = new HubEngine({ authMode: "none" });
    localEngines.set(stateDir, engine);
  }
  return engine;
}

export function toHttpHubUrl(rawUrl: string): string {
  const trimmed = rawUrl.trim();
  if (trimmed === "local" || trimmed === "local://") return "local://";
  const u = new URL(trimmed);
  if (u.protocol === "nats:") {
    return `http://${u.hostname}:${u.port || 4222}`;
  }
  if (u.protocol === "ws:") {
    return `http://${u.host}`;
  }
  if (u.protocol === "wss:") {
    return `https://${u.host}`;
  }
  if (u.protocol === "http:" || u.protocol === "https:") {
    const cleanPath = u.pathname === "/" ? "" : u.pathname.replace(/\/+$/, "");
    return `${u.protocol}//${u.host}${cleanPath}`;
  }
  throw new Error(`Unsupported hub URL: ${rawUrl}`);
}

function makeKvEntry(raw: {
  key: string;
  revision: number;
  created_ms: number;
  value_b64: string;
  operation: "PUT" | "DEL" | "PURGE";
}, delta = 0): KvEntry {
  const value = new Uint8Array(Buffer.from(raw.value_b64, "base64"));
  const created = new Date(raw.created_ms);
  return {
    bucket: LISTENERS_BUCKET,
    key: raw.key,
    value,
    created,
    revision: raw.revision,
    delta,
    operation: raw.operation,
    json<T = unknown>(): T {
      return JSON.parse(decoder.decode(value)) as T;
    },
    string(): string {
      return decoder.decode(value);
    },
  };
}

interface InternalTransport {
  rpc(op: string, params?: Record<string, unknown>): Promise<unknown>;
  events: EventEmitter;
  reconnectSse?(): Promise<void>;
  close(): Promise<void>;
}

function buildConnection(transport: InternalTransport, onAuthFailureRef: { fn?: (code: string) => void }): NatsConnection {
  let isClosedFlag = false;
  let closedError: Error | undefined;
  let resolveClosed!: (err: void | Error) => void;
  const closedPromise = new Promise<void | Error>(resolve => {
    resolveClosed = resolve;
  });
  const statusIter = new SimpleQueuedIterator<{ type: string; data?: unknown }>();
  const inflightPublishes = new Set<Promise<void>>();
  const activeIterators = new Set<{ stop(err?: Error): void }>();
  const ncHandlers = new Set<() => void>();

  onAuthFailureRef.fn = (code: string) => {
    if (isClosedFlag) return;
    closedError = Object.assign(new Error(code), { code });
    statusIter.push({ type: Events.Error, data: code });
  };

  const ensureOpen = () => {
    if (isClosedFlag) {
      throw new CitoError("NATS_UNAVAILABLE", "Connection is closed");
    }
  };

  const rpc = async <T = unknown>(op: string, params: Record<string, unknown> = {}): Promise<T> => {
    ensureOpen();
    return (await transport.rpc(op, params)) as T;
  };

  const kvView: KvView = {
    async get(key: string, opts?: { checkForbiddenScope?: boolean }): Promise<KvEntry | null> {
      const res = await rpc<
        | { key: string; revision: number; created_ms: number; value_b64: string; operation: "PUT" }
        | { forbidden_scope: true }
        | null
      >("kv_get", { key });
      if (!res) return null;
      if ("forbidden_scope" in res && res.forbidden_scope) {
        if (opts?.checkForbiddenScope) {
          throw new CitoError("FORBIDDEN_TARGET_SCOPE", "Target is private to its principal");
        }
        return null;
      }
      return makeKvEntry(res as { key: string; revision: number; created_ms: number; value_b64: string; operation: "PUT" });
    },
    async create(key: string, data: Uint8Array): Promise<number> {
      const res = await rpc<{ revision: number }>("kv_create", {
        key,
        data_b64: Buffer.from(data).toString("base64"),
      });
      return res.revision;
    },
    async update(key: string, data: Uint8Array, expectedRevision: number): Promise<number> {
      const res = await rpc<{ revision: number }>("kv_update", {
        key,
        data_b64: Buffer.from(data).toString("base64"),
        revision: expectedRevision,
      });
      return res.revision;
    },
    async put(key: string, data: Uint8Array): Promise<number> {
      const res = await rpc<{ revision: number }>("kv_put", {
        key,
        data_b64: Buffer.from(data).toString("base64"),
      });
      return res.revision;
    },
    async delete(key: string): Promise<void> {
      await rpc("kv_delete", { key });
    },
    async purge(key: string): Promise<void> {
      await rpc("kv_delete", { key });
    },
    async keys(): Promise<QueuedIterator<string>> {
      const res = await rpc<{ keys: string[] }>("kv_keys");
      const iter = new SimpleQueuedIterator<string>(() => {
        activeIterators.delete(iter);
      });
      activeIterators.add(iter);
      for (const k of res.keys) {
        iter.push(k);
      }
      iter.stop();
      return iter;
    },
    async watch(opts?: {
      key?: string;
      ignoreDeletes?: boolean;
      initializedFn?: () => void;
    }): Promise<QueuedIterator<KvEntry>> {
      const keyFilter = opts?.key;
      let iter!: SimpleQueuedIterator<KvEntry>;
      const liveBuffer: { key: string; revision: number; created_ms: number; value_b64: string; operation: "PUT" | "DEL" | "PURGE" }[] = [];
      let snapshotLoaded = false;
      const seenRevs = new Map<string, number>();

      const onKv = (ev: { key: string; revision: number; created_ms: number; value_b64: string; operation: "PUT" | "DEL" | "PURGE" }) => {
        if (keyFilter && !subjectMatchesFilter(ev.key, keyFilter)) return;
        if (opts?.ignoreDeletes && (ev.operation === "DEL" || ev.operation === "PURGE")) return;
        if (!snapshotLoaded) {
          liveBuffer.push(ev);
          return;
        }
        const prevRev = seenRevs.get(ev.key) ?? 0;
        if (ev.revision <= prevRev) return;
        seenRevs.set(ev.key, ev.revision);
        iter.push(makeKvEntry(ev, 0));
      };

      transport.events.on("kv", onKv);
      iter = new SimpleQueuedIterator<KvEntry>(() => {
        transport.events.off("kv", onKv);
        activeIterators.delete(iter);
      });
      activeIterators.add(iter);

      try {
        const snap = await rpc<{
          entries: { key: string; revision: number; created_ms: number; value_b64: string; operation: "PUT" }[];
        }>("kv_snapshot", { ...(keyFilter ? { key: keyFilter } : {}) });
        const entries = snap.entries;
        for (let i = 0; i < entries.length; i++) {
          const item = entries[i]!;
          seenRevs.set(item.key, item.revision);
          const delta = entries.length - 1 - i;
          iter.push(makeKvEntry(item, delta));
        }
        snapshotLoaded = true;
        for (const buffered of liveBuffer.splice(0)) {
          const prevRev = seenRevs.get(buffered.key) ?? 0;
          if (buffered.revision <= prevRev) continue;
          seenRevs.set(buffered.key, buffered.revision);
          iter.push(makeKvEntry(buffered, 0));
        }
        opts?.initializedFn?.();
        return iter;
      } catch (error) {
        transport.events.off("kv", onKv);
        activeIterators.delete(iter);
        throw error;
      }
    },
  };

  const jsm: JetStreamManager = {
    async getAccountInfo() {
      return await rpc<{ streams: number; consumers: number; memory: number; storage: number }>("account_info");
    },
    streams: {
      async info(stream: string) {
        return await rpc<{
          config: StreamConfig;
          state: { messages: number; last_seq: number; first_seq: number; bytes: number; consumer_count: number };
          ts: string;
        }>("stream_info", { stream });
      },
      async add(config: Partial<StreamConfig> & { name: string }) {
        return await rpc("stream_add", { config });
      },
      async update(stream: string, patch: Partial<StreamConfig>) {
        return await rpc("stream_update", { stream, patch });
      },
      async getMessage(stream: string, query: { seq: number }) {
        const res = await rpc<{ seq: number; subject: string; created_ms: number; data_b64: string }>(
          "stream_get_message",
          { stream, seq: query.seq },
        );
        const data = new Uint8Array(Buffer.from(res.data_b64, "base64"));
        return {
          seq: res.seq,
          subject: res.subject,
          data,
          time: new Date(res.created_ms),
          json<T = unknown>(): T {
            return JSON.parse(decoder.decode(data)) as T;
          },
          string(): string {
            return decoder.decode(data);
          },
        };
      },
    },
    consumers: {
      async add(stream: string, config: Record<string, unknown>) {
        return await rpc<{ name: string; stream_name: string; config: Record<string, unknown> }>("consumer_add", {
          stream,
          config,
        });
      },
      async delete(stream: string, name: string) {
        await rpc("consumer_delete", { stream, name });
        return true;
      },
    },
  };

  const makeJsMsg = (
    consumerName: string,
    item: { seq: number; subject: string; created_ms: number; data_b64: string; acked_at_ms?: number | null },
  ): JsMsg => {
    const data = new Uint8Array(Buffer.from(item.data_b64, "base64"));
    return {
      seq: item.seq,
      subject: item.subject,
      data,
      info: {
        timestampNanos: Number(item.created_ms) * 1_000_000,
        stream: SIGNALS_STREAM,
        consumer: consumerName,
        ackedAtMs: item.acked_at_ms ?? null,
      },
      json<T = unknown>(): T {
        return JSON.parse(decoder.decode(data)) as T;
      },
      string(): string {
        return decoder.decode(data);
      },
      ack(): void {
        void rpc("consumer_ack", { stream: SIGNALS_STREAM, name: consumerName, seq: item.seq }).catch(() => {});
      },
      ackAck(): Promise<boolean> {
        return rpc("consumer_ack", { stream: SIGNALS_STREAM, name: consumerName, seq: item.seq }).then(
          () => true,
          () => false,
        );
      },
      term(_reason?: string): void {
        void rpc("consumer_ack", { stream: SIGNALS_STREAM, name: consumerName, seq: item.seq }).catch(() => {});
      },
      working(): void {},
    };
  };

  const js: JetStreamClient = {
    async publish(subject: string, data: Uint8Array = new Uint8Array(), opts?: { msgID?: string }) {
      return await rpc<{ seq: number; duplicate: boolean; stream: string }>("js_publish", {
        subject,
        data_b64: Buffer.from(data).toString("base64"),
        ...(opts ? { opts } : {}),
      });
    },
    views: {
      async kv(_bucket: string, _opts?: Record<string, unknown>): Promise<KvView> {
        return kvView;
      },
    },
    consumers: {
      async get(stream: string, name: string): Promise<Consumer> {
        return {
          async consume(_opts?: { max_messages?: number }): Promise<ConsumerMessages> {
            const seenSeqs = new Set<number>();
            let filterSubject: string | undefined;
            let startSeq = 1;
            let backlogLoaded = false;
            const liveBuffer: { seq: number; subject: string; created_ms: number; data_b64: string }[] = [];

            let iter!: SimpleQueuedIterator<JsMsg>;
            const onSignal = (ev: { seq: number; subject: string; created_ms: number; data_b64: string }) => {
              if (!backlogLoaded) {
                liveBuffer.push(ev);
                return;
              }
              if (ev.seq < startSeq || seenSeqs.has(ev.seq)) return;
              if (!subjectMatchesFilter(ev.subject, filterSubject)) return;
              seenSeqs.add(ev.seq);
              iter.push(makeJsMsg(name, ev));
            };

            transport.events.on("signal", onSignal);
            iter = new SimpleQueuedIterator<JsMsg>(() => {
              transport.events.off("signal", onSignal);
              activeIterators.delete(iter);
            });
            activeIterators.add(iter);

            try {
              const info = await rpc<{
                filterSubject?: string;
                startSeq: number;
                backlog: { seq: number; subject: string; created_ms: number; data_b64: string }[];
              }>("consumer_backlog", { stream, name });
              filterSubject = info.filterSubject;
              startSeq = info.startSeq;
              for (const item of info.backlog) {
                if (item.seq < startSeq || seenSeqs.has(item.seq)) continue;
                seenSeqs.add(item.seq);
                iter.push(makeJsMsg(name, item));
              }
              backlogLoaded = true;
              for (const ev of liveBuffer.splice(0)) {
                if (ev.seq < startSeq || seenSeqs.has(ev.seq)) continue;
                if (!subjectMatchesFilter(ev.subject, filterSubject)) continue;
                seenSeqs.add(ev.seq);
                iter.push(makeJsMsg(name, ev));
              }
            } catch (error) {
              transport.events.off("signal", onSignal);
              activeIterators.delete(iter);
              throw error;
            }

            return {
              [Symbol.asyncIterator](): AsyncIterator<JsMsg> {
                return iter[Symbol.asyncIterator]();
              },
              async close(): Promise<void> {
                iter.stop();
              },
              stop(): void {
                iter.stop();
              },
            };
          },
          async next(opts?: { expires?: number }): Promise<JsMsg | null> {
            const item = await rpc<{ seq: number; subject: string; created_ms: number; data_b64: string } | null>(
              "consumer_next",
              { stream, name, expires: opts?.expires ?? 1000 },
            );
            if (!item) return null;
            return makeJsMsg(name, item);
          },
          async delete(): Promise<boolean> {
            await rpc("consumer_delete", { stream, name });
            return true;
          },
        };
      },
    },
  };

  const nc: NatsConnection = {
    rpc,
    publish(subject: string, data: Uint8Array = new Uint8Array()): void {
      if (isClosedFlag) return;
      const p = rpc("nc_publish", {
        subject,
        data_b64: Buffer.from(data).toString("base64"),
      }).then(
        () => {
          inflightPublishes.delete(p);
        },
        err => {
          inflightPublishes.delete(p);
          throw err;
        },
      );
      void p.catch(() => {});
      inflightPublishes.add(p);
    },
    subscribe(
      subject: string,
      opts?: { queue?: string; callback?: (error: Error | null, msg: Msg) => void },
    ): Subscription {
      if (!isAllowedCitoSubject(subject)) {
        const permErr = Object.assign(new Error("PERMISSIONS_VIOLATION"), { code: ErrorCode.PermissionsViolation });
        queueMicrotask(() => {
          onAuthFailureRef.fn?.(ErrorCode.PermissionsViolation);
          opts?.callback?.(permErr, undefined as unknown as Msg);
        });
        return { unsubscribe() {} };
      }
      const onNc = (ev: { subject: string; data_b64: string }) => {
        if (!subjectMatchesFilter(ev.subject, subject)) return;
        const data = new Uint8Array(Buffer.from(ev.data_b64, "base64"));
        const msg: Msg = {
          subject: ev.subject,
          data,
          json<T = unknown>(): T {
            return JSON.parse(decoder.decode(data)) as T;
          },
          string(): string {
            return decoder.decode(data);
          },
          respond(): boolean {
            return false;
          },
        };
        opts?.callback?.(null, msg);
      };
      transport.events.on("nc", onNc);
      const unsub = () => {
        transport.events.off("nc", onNc);
        ncHandlers.delete(unsub);
      };
      ncHandlers.add(unsub);
      return { unsubscribe: unsub };
    },
    async flush(): Promise<void> {
      ensureOpen();
      if (inflightPublishes.size > 0) {
        const pending = [...inflightPublishes];
        const results = await Promise.allSettled(pending);
        const rejected = results.find((r): r is PromiseRejectedResult => r.status === "rejected");
        if (rejected) throw rejected.reason;
      }
    },
    async close(): Promise<void> {
      if (isClosedFlag) return;
      isClosedFlag = true;
      for (const unsub of [...ncHandlers]) unsub();
      for (const iter of [...activeIterators]) iter.stop();
      await transport.close();
      resolveClosed(closedError);
      statusIter.stop();
    },
    async drain(): Promise<void> {
      await nc.flush().catch(() => {});
      await nc.close();
    },
    isClosed(): boolean {
      return isClosedFlag;
    },
    closed(): Promise<void | Error> {
      return closedPromise;
    },
    status(): QueuedIterator<{ type: string; data?: unknown }> {
      return statusIter;
    },
    async reconnect(): Promise<void> {
      ensureOpen();
      statusIter.push({ type: Events.Disconnect });
      await new Promise(resolve => setTimeout(resolve, 15));
      if (isClosedFlag) return;
      await transport.reconnectSse?.();
      if (isClosedFlag) return;
      statusIter.push({ type: Events.Reconnect });
      await new Promise(resolve => setTimeout(resolve, 15));
    },
    jetstream(): JetStreamClient {
      return js;
    },
    async jetstreamManager(): Promise<JetStreamManager> {
      return jsm;
    },
  };

  return nc;
}

export function createLocalNatsConnection(options: {
  engine?: HubEngine;
  stateDir?: string;
  user?: string;
} = {}): NatsConnection {
  const engine = options.engine ?? getLocalHubEngine(options.stateDir);
  const principal: Principal = localPrincipal(options.user ?? process.env.CITO_USER);
  const events = new EventEmitter();
  events.setMaxListeners(0);
  const onAuthFailureRef: { fn?: (code: string) => void } = {};

  const onKv = (ev: HubKvEvent) => {
    if (!engine.canReadKvRow(ev, principal)) return;
    events.emit("kv", {
      key: ev.key,
      revision: ev.revision,
      created_ms: ev.created_ms,
      value_b64: ev.value.toString("base64"),
      operation: ev.operation,
    });
  };
  const onSignal = (ev: HubSignalEvent) => {
    events.emit("signal", {
      seq: ev.seq,
      subject: ev.subject,
      created_ms: ev.created_ms,
      data_b64: ev.data.toString("base64"),
    });
  };
  const onNc = (ev: HubNcEvent) => {
    events.emit("nc", {
      subject: ev.subject,
      data_b64: ev.data.toString("base64"),
    });
  };

  engine.events.on("kv", onKv);
  engine.events.on("signal", onSignal);
  engine.events.on("nc", onNc);

  const transport: InternalTransport = {
    events,
    async rpc(op: string, params: Record<string, unknown> = {}): Promise<unknown> {
      await Promise.resolve();
      try {
        return await engine.handleRpc(op, params, principal);
      } catch (error) {
        if (error instanceof HubError) {
          if (error.code === "FORBIDDEN_TARGET_SCOPE") {
            throw new CitoError("FORBIDDEN_TARGET_SCOPE", error.message);
          }
          if (error.status === 401 || error.status === 403) {
            onAuthFailureRef.fn?.(error.code);
          }
          throw Object.assign(new Error(error.message), {
            code: error.code,
            ...(error.api_error ? { api_error: error.api_error } : {}),
          });
        }
        throw error;
      }
    },
    async close(): Promise<void> {
      engine.events.off("kv", onKv);
      engine.events.off("signal", onSignal);
      engine.events.off("nc", onNc);
      events.removeAllListeners();
    },
  };

  return buildConnection(transport, onAuthFailureRef);
}

export async function createRemoteNatsConnection(options: {
  url: string;
  token?: string;
  user?: string;
  timeout?: number;
}): Promise<NatsConnection> {
  const baseUrl = toHttpHubUrl(options.url);
  const timeoutMs = options.timeout ?? 10_000;
  const events = new EventEmitter();
  events.setMaxListeners(0);
  const onAuthFailureRef: { fn?: (code: string) => void } = {};

  const buildHeaders = (extra: Record<string, string> = {}): Record<string, string> => {
    const headers: Record<string, string> = {
      "x-cito-user": options.user ?? process.env.CITO_USER ?? "local",
      ...extra,
    };
    if (options.token) {
      headers.authorization = `Bearer ${options.token}`;
    }
    return headers;
  };

  let closeSse: (() => void) | undefined;

  const connectSse = (): Promise<void> =>
    new Promise<void>((resolve, reject) => {
      const target = new URL(baseUrl + "/v1/events");
      const reqFn = target.protocol === "https:" ? httpsRequest : httpRequest;
      let settled = false;
      const timer = setTimeout(() => {
        if (settled) return;
        settled = true;
        req.destroy();
        reject(Object.assign(new Error("Hub SSE connection timed out"), { code: "NATS_UNAVAILABLE" }));
      }, timeoutMs);
      timer.unref?.();

      const req = reqFn(
        target,
        {
          method: "GET",
          headers: buildHeaders({ accept: "text/event-stream" }),
        },
        res => {
          if (res.statusCode === 401 || res.statusCode === 403) {
            clearTimeout(timer);
            settled = true;
            res.resume();
            reject(Object.assign(new Error("Authorization Violation"), { code: ErrorCode.AuthorizationViolation }));
            return;
          }
          if (res.statusCode === 503) {
            clearTimeout(timer);
            settled = true;
            res.resume();
            reject(new CitoError("AUTH_UNAVAILABLE", "Authentication service unavailable"));
            return;
          }
          if (res.statusCode !== 200) {
            clearTimeout(timer);
            settled = true;
            res.resume();
            reject(Object.assign(new Error(`Hub SSE HTTP ${res.statusCode}`), { code: "NATS_UNAVAILABLE" }));
            return;
          }

          const strDec = new StringDecoder("utf8");
          let buffer = "";
          res.on("data", (chunk: Buffer) => {
            buffer += strDec.write(chunk);
            let idx: number;
            while ((idx = buffer.indexOf("\n\n")) >= 0) {
              const frame = buffer.slice(0, idx);
              buffer = buffer.slice(idx + 2);
              for (const line of frame.split("\n")) {
                if (line.startsWith(":ok")) {
                  if (!settled) {
                    settled = true;
                    clearTimeout(timer);
                    resolve();
                  }
                } else if (line.startsWith("data:")) {
                  const rawJson = line.slice(5).trim();
                  if (!rawJson) continue;
                  try {
                    const ev = JSON.parse(rawJson) as { type?: string };
                    if (ev.type === "kv" || ev.type === "signal" || ev.type === "nc") {
                      events.emit(ev.type, ev);
                    }
                  } catch {}
                }
              }
            }
          });
          res.on("error", () => {});
        },
      );

      req.on("error", err => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        reject(Object.assign(new Error("Cannot connect to Hub: " + err.message), { code: "NATS_UNAVAILABLE", cause: err }));
      });
      req.end();

      closeSse = () => {
        clearTimeout(timer);
        req.destroy();
      };
    });

  const httpAgent = new HttpAgent({ keepAlive: true, maxSockets: 1 });
  const httpsAgent = new HttpsAgent({ keepAlive: true, maxSockets: 1 });

  const rpc = (op: string, params: Record<string, unknown> = {}): Promise<unknown> =>
    new Promise((resolve, reject) => {
      const target = new URL(baseUrl + "/v1/rpc");
      const isHttps = target.protocol === "https:";
      const reqFn = isHttps ? httpsRequest : httpRequest;
      const payloadStr = JSON.stringify({ op, ...params });
      let settled = false;
      const timer = setTimeout(() => {
        if (settled) return;
        settled = true;
        req.destroy();
        reject(Object.assign(new Error("Hub RPC timed out"), { code: "NATS_UNAVAILABLE" }));
      }, timeoutMs);
      timer.unref?.();

      const req = reqFn(
        target,
        {
          method: "POST",
          agent: isHttps ? httpsAgent : httpAgent,
          headers: buildHeaders({
            "content-type": "application/json",
            "content-length": String(Buffer.byteLength(payloadStr, "utf8")),
          }),
        },
        res => {
          const chunks: Buffer[] = [];
          res.on("data", (chunk: Buffer) => chunks.push(chunk));
          res.on("end", () => {
            if (settled) return;
            settled = true;
            clearTimeout(timer);
            const status = res.statusCode ?? 500;
            let body: {
              ok?: boolean;
              result?: unknown;
              error?: { code?: string; message?: string; api_error?: { err_code: number } };
            } = {};
            try {
              body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
            } catch {}
            if (status < 200 || status >= 300 || body.ok === false) {
              const code =
                body.error?.code ??
                (status === 401
                  ? ErrorCode.AuthorizationViolation
                  : status === 403
                  ? ErrorCode.PermissionsViolation
                  : "NATS_UNAVAILABLE");
              const msg = body.error?.message ?? `Hub RPC HTTP ${status}`;
              if (code === "FORBIDDEN_TARGET_SCOPE") {
                reject(new CitoError("FORBIDDEN_TARGET_SCOPE", msg));
                return;
              }
              if (status === 503 || code === "AUTH_UNAVAILABLE") {
                reject(new CitoError("AUTH_UNAVAILABLE", msg));
                return;
              }
              if (status === 401 || status === 403) {
                onAuthFailureRef.fn?.(code);
              }
              reject(
                Object.assign(new Error(msg), {
                  code,
                  ...(body.error?.api_error ? { api_error: body.error.api_error } : {}),
                }),
              );
              return;
            }
            resolve(body.result);
          });
          res.on("error", err => {
            if (settled) return;
            settled = true;
            clearTimeout(timer);
            reject(Object.assign(new Error("Hub RPC failed: " + err.message), { code: "NATS_UNAVAILABLE", cause: err }));
          });
        },
      );
      req.on("error", err => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        reject(Object.assign(new Error("Hub RPC failed: " + err.message), { code: "NATS_UNAVAILABLE", cause: err }));
      });
      req.end(payloadStr);
    });

  await connectSse();
  try {
    await rpc("stream_info", { stream: KV_LISTENERS_STREAM });
  } catch (error) {
    closeSse?.();
    throw error;
  }

  const transport: InternalTransport = {
    events,
    rpc,
    async reconnectSse(): Promise<void> {
      closeSse?.();
      await connectSse();
    },
    async close(): Promise<void> {
      closeSse?.();
      httpAgent.destroy();
      httpsAgent.destroy();
      events.removeAllListeners();
    },
  };

  return buildConnection(transport, onAuthFailureRef);
}

export async function connectLocalHub(options: {
  engine?: HubEngine;
  stateDir?: string;
  user?: string;
} = {}): Promise<Broker> {
  const nc = createLocalNatsConnection(options);
  const js = nc.jetstream();
  const jsm = await nc.jetstreamManager();
  const kv = await js.views.kv(LISTENERS_BUCKET);
  return { nc, js, jsm, kv };
}

export async function connectRemoteHub(options: {
  url: string;
  token?: string;
  user?: string;
  timeout?: number;
}): Promise<Broker> {
  const nc = await createRemoteNatsConnection(options);
  const js = nc.jetstream();
  const jsm = await nc.jetstreamManager();
  const kv = await js.views.kv(LISTENERS_BUCKET);
  return { nc, js, jsm, kv };
}

export { DEFAULT_KV_STREAM_CONFIG, DEFAULT_SIGNALS_STREAM_CONFIG };
