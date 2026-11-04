import {
  connect, DiscardPolicy, RetentionPolicy, StorageType,
  type ConnectionOptions, type NatsConnection, type StreamConfig, type JetStreamManager,
} from "nats";
import { RETENTION_MS } from "./lease.js";

/** Node TCP and Node's native WebSocket use the same NATS v2 public API. */
export async function connectNats(options: ConnectionOptions): Promise<NatsConnection> {
  const servers = typeof options.servers === "string" ? [options.servers] : options.servers ?? [];
  if (servers.some(server => /^wss?:/.test(server))) {
    if (!servers.every(server => /^wss?:/.test(server))) throw new Error("Cannot mix TCP and WebSocket NATS servers");
    const ws = await import("nats.ws");
    return ws.connect(options);
  }
  return connect(options);
}

const LISTENERS_BUCKET = "cito_listeners";
export const SIGNALS_STREAM = "CITO_SIGNALS";
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
  return typeof error === "object" && error !== null
    && "api_error" in error && typeof error.api_error === "object" && error.api_error !== null
    && "err_code" in error.api_error && error.api_error.err_code === code;
}
function assertConfig(actual: StreamConfig, expected: Partial<StreamConfig>): void {
  for (const [key, value] of Object.entries(expected)) {
    const found = actual[key as keyof StreamConfig];
    if (JSON.stringify(found) !== JSON.stringify(value)) {
      throw new Error("Incompatible NATS stream " + actual.name + ": " + key
        + " expected " + JSON.stringify(value) + ", got " + JSON.stringify(found));
    }
  }
}
async function ensureSignals(jsm: JetStreamManager): Promise<void> {
  try { await jsm.streams.info(SIGNALS_STREAM); }
  catch (error) {
    if (!isJsApiError(error, 10059)) throw error;
    // The server accepts concurrent identical stream creations. If another client
    // wins with a different config, the final verification rejects the mismatch.
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
    history: 1, ttl: 0, storage: StorageType.File,
  });
  assertConfig((await jsm.streams.info("KV_" + LISTENERS_BUCKET)).config, {
    subjects: ["$KV.cito_listeners.>"],
    max_msgs_per_subject: 1, max_age: 0,
    retention: RetentionPolicy.Limits, storage: StorageType.File,
  });
  return { js, jsm, kv };
}

/** Broker time is required; never silently fall back to the local wall clock. */
export async function brokerNow(jsm: JetStreamManager, stream = "KV_" + LISTENERS_BUCKET): Promise<string> {
  const info = await jsm.streams.info(stream);
  if (!info.ts || !Number.isFinite(Date.parse(info.ts))) {
    throw new Error("NATS >= 2.10 with broker StreamInfo.ts is required");
  }
  return info.ts;
}
