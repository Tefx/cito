import {
  AckPolicy,
  DeliverPolicy,
  DiscardPolicy,
  ErrorCode,
  Events,
  KV_LISTENERS_STREAM,
  LISTENERS_BUCKET,
  RetentionPolicy,
  SIGNALS_STREAM,
  StorageType,
  StringCodec,
  createLocalNatsConnection,
  createRemoteNatsConnection,
  toHttpHubUrl,
  type Broker,
  type ConnectionOptions,
  type Consumer,
  type ConsumerMessages,
  type JetStreamClient,
  type JetStreamManager,
  type JsMsg,
  type KvEntry,
  type KvView,
  type Msg,
  type NatsConnection,
  type QueuedIterator,
  type StreamConfig,
  type Subscription,
} from "../hub/client.js";
import { RETENTION_MS } from "./lease.js";

export {
  AckPolicy,
  DeliverPolicy,
  DiscardPolicy,
  ErrorCode as NatsErrorCode,
  Events,
  KV_LISTENERS_STREAM,
  LISTENERS_BUCKET,
  RetentionPolicy,
  SIGNALS_STREAM,
  StorageType,
  StringCodec,
  type Broker,
  type ConnectionOptions,
  type Consumer,
  type ConsumerMessages,
  type JetStreamClient,
  type JetStreamManager,
  type JsMsg,
  type KvEntry,
  type KvView,
  type Msg,
  type NatsConnection,
  type QueuedIterator,
  type StreamConfig,
  type Subscription,
};

export async function connectNats(options: ConnectionOptions = {}): Promise<NatsConnection> {
  const servers = typeof options.servers === "string" ? [options.servers] : options.servers ?? ["local://"];
  if (servers.some(server => /^wss?:/.test(server)) && !servers.every(server => /^wss?:/.test(server))) {
    throw new Error("Cannot mix TCP and WebSocket Hub servers");
  }
  const first = servers[0] ?? "local://";
  const normalized = toHttpHubUrl(first);
  if (normalized === "local://") {
    return createLocalNatsConnection({
      stateDir: options.stateDir,
      user: options.user,
    });
  }
  return await createRemoteNatsConnection({
    url: normalized,
    token: options.token,
    user: options.user,
    timeout: options.timeout,
  });
}

export const connect = connectNats;

const SIGNALS_STREAM_CONFIG: Partial<StreamConfig> & { name: string } = {
  name: SIGNALS_STREAM,
  subjects: ["cito.signal.*"],
  retention: RetentionPolicy.Limits,
  max_age: RETENTION_MS * 1_000_000,
  duplicate_window: RETENTION_MS * 1_000_000,
  storage: StorageType.File,
  discard: DiscardPolicy.Old,
};

export function isJsApiError(error: unknown, code: number): boolean {
  if (typeof error !== "object" || error === null) return false;
  const e = error as { code?: unknown; api_error?: { err_code?: unknown } };
  return e.api_error?.err_code === code || Number(e.code) === code;
}

function assertConfig(actual: StreamConfig, expected: Partial<StreamConfig>): void {
  for (const [key, value] of Object.entries(expected)) {
    const found = actual[key as keyof StreamConfig];
    if (JSON.stringify(found) !== JSON.stringify(value)) {
      throw new Error(
        "Incompatible Hub stream " +
          actual.name +
          ": " +
          key +
          " expected " +
          JSON.stringify(value) +
          ", got " +
          JSON.stringify(found),
      );
    }
  }
}

async function ensureSignals(jsm: JetStreamManager): Promise<void> {
  try {
    await jsm.streams.info(SIGNALS_STREAM);
  } catch (error) {
    if (!isJsApiError(error, 10059)) throw error;
    await jsm.streams.add(SIGNALS_STREAM_CONFIG);
  }
  assertConfig((await jsm.streams.info(SIGNALS_STREAM)).config, SIGNALS_STREAM_CONFIG);
}

/** No destructive repair: reject incompatible existing topology without changing it. */
export async function provisionNats(nc: NatsConnection) {
  const js = nc.jetstream();
  const jsm = await nc.jetstreamManager();
  await ensureSignals(jsm);
  const kv = await js.views.kv(LISTENERS_BUCKET, {
    history: 1,
    ttl: 0,
    storage: StorageType.File,
  });
  assertConfig((await jsm.streams.info("KV_" + LISTENERS_BUCKET)).config, {
    subjects: ["$KV.cito_listeners.>"],
    max_msgs_per_subject: 1,
    max_age: 0,
    retention: RetentionPolicy.Limits,
    storage: StorageType.File,
  });
  return { js, jsm, kv };
}

/** Broker time is required; never silently fall back to the local wall clock. */
export async function brokerNow(jsm: JetStreamManager, stream = "KV_" + LISTENERS_BUCKET): Promise<string> {
  const info = await jsm.streams.info(stream);
  if (!info.ts || !Number.isFinite(Date.parse(info.ts))) {
    throw new Error("Hub broker StreamInfo.ts is required");
  }
  return info.ts;
}
