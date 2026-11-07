import { randomBytes, randomUUID } from "node:crypto";
import { EventEmitter } from "node:events";
import { appendFileSync, chmodSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { evaluateLease, evaluateNodeLease, RETENTION_MS, type ChannelAttachment } from "../shared/lease.js";
import { humanNodeKey, listenerId, localPrincipal, publicListenerId, toCanonicalListenerId, type Principal } from "../shared/identity.js";
import type { HubAuthMode } from "../shared/config.js";
import type {
  AdapterTint,
  HumanChannelPlugin,
  HumanChannelServiceContext,
  HumanNotificationPayload,
} from "../adapters/types.js";

export const HUMAN_CHANNEL_REGISTRY = new Map<string, HumanChannelPlugin>();
export const rejectedHumanPluginIds = new Set<string>();

export function getHumanChannelPlugins(): HumanChannelPlugin[] {
  const items: HumanChannelPlugin[] = [];
  for (const plugin of HUMAN_CHANNEL_REGISTRY.values()) {
    if (rejectedHumanPluginIds.has(plugin.id)) continue;
    items.push(plugin);
  }
  return items;
}

export function getHumanChannelPlugin(id: string): HumanChannelPlugin | undefined {
  if (rejectedHumanPluginIds.has(id)) return undefined;
  return HUMAN_CHANNEL_REGISTRY.get(id);
}

export const LISTENERS_BUCKET = "cito_listeners";
export const KV_LISTENERS_STREAM = "KV_cito_listeners";
export const SIGNALS_STREAM = "CITO_SIGNALS";

export interface StreamConfig {
  name: string;
  subjects: string[];
  retention: string;
  max_age: number;
  duplicate_window?: number;
  max_msgs_per_subject?: number;
  storage: string;
  discard: string;
  [key: string]: unknown;
}

export const DEFAULT_SIGNALS_STREAM_CONFIG: StreamConfig = {
  name: SIGNALS_STREAM,
  subjects: ["cito.signal.*"],
  retention: "limits",
  max_age: RETENTION_MS * 1_000_000,
  duplicate_window: RETENTION_MS * 1_000_000,
  storage: "file",
  discard: "old",
};

export const DEFAULT_KV_STREAM_CONFIG: StreamConfig = {
  name: KV_LISTENERS_STREAM,
  subjects: ["$KV.cito_listeners.>"],
  max_msgs_per_subject: 1,
  max_age: 0,
  retention: "limits",
  storage: "file",
  discard: "old",
};

export class HubError extends Error {
  readonly status: number;
  readonly code: string;
  readonly api_error?: { err_code: number };
  constructor(status: number, code: string, message: string, apiErrorCode?: number) {
    super(message);
    this.name = "HubError";
    this.status = status;
    this.code = code;
    if (apiErrorCode !== undefined) {
      this.api_error = { err_code: apiErrorCode };
    }
  }
}

export interface HubAuditEvent {
  ts_ms: number;
  principal_id: string;
  action: "register" | "unlisten" | "send" | "ack";
  target_id: string;
  listener_id?: string;
  signal_id?: string;
  detail?: Record<string, unknown>;
}

export interface HubKvEvent {
  key: string;
  revision: number;
  created_ms: number;
  value: Buffer;
  operation: "PUT" | "DEL" | "PURGE";
  principal_id: string | null;
  scope: string | null;
}

export interface HubSignalEvent {
  seq: number;
  subject: string;
  created_ms: number;
  data: Buffer;
  acked_at_ms?: number | null;
}

export interface HubNcEvent {
  subject: string;
  data: Buffer;
}

export interface UiChannelFieldSummary {
  key: string;
  scope: "hub" | "user";
  label: string;
  type: "string" | "secret" | "url" | "boolean";
  required: boolean;
  placeholder: string;
  value: string;
  configured: boolean;
  hidden?: boolean;
}

export interface UiChannelSetupSummary {
  id: string;
  display_name: string;
  sf_symbol: string;
  tint: AdapterTint;
  enabled: boolean;
  configured: boolean;
  status: "ready" | "hub_ready" | "not_configured" | "error";
  detail: string;
  display_label?: string | null;
  hub_provided: boolean;
  pairing_supported: boolean;
  pairing_action_label?: string | null;
  pairing_code?: string | null;
  pairing_instruction?: string | null;
  fields: UiChannelFieldSummary[];
}

export interface HubEngineOptions {
  dataDir?: string;
  authMode?: HubAuthMode;
  auditLogPath?: string;
  onAudit?: (event: HubAuditEvent) => void;
  now?: () => number;
  env?: NodeJS.ProcessEnv;
  fetchFn?: typeof fetch;
}

interface ConsumerState {
  name: string;
  stream: string;
  filterSubject?: string;
  startSeq: number;
  cursorSeq: number;
  ackedSeqs: Set<number>;
  principalId: string | null;
}

function encodeBase32(bytes: Uint8Array): string {
  const alphabet = "abcdefghijklmnopqrstuvwxyz234567";
  let bits = 0;
  let value = 0;
  let encoded = "";
  for (const byte of bytes) {
    value = (value << 8) | byte;
    bits += 8;
    while (bits >= 5) {
      bits -= 5;
      encoded += alphabet[(value >>> bits) & 31];
    }
  }
  if (bits > 0) {
    encoded += alphabet[(value << (5 - bits)) & 31];
  }
  return encoded;
}

export function generatePairingCode(): string {
  return "cito-" + encodeBase32(randomBytes(6)).slice(0, 6);
}

export function isAllowedCitoSubject(subject: string): boolean {
  if (!subject || typeof subject !== "string") return false;
  return /^cito\.(?:signal|ack)\.[A-Za-z0-9_.*-]+$/.test(subject);
}

export function subjectMatchesFilter(subject: string, filter?: string): boolean {
  if (!filter || filter === ">" || filter === "*") return true;
  if (filter === subject) return true;
  if (filter.endsWith(".*")) {
    const prefix = filter.slice(0, -1);
    return subject.startsWith(prefix) && !subject.slice(prefix.length).includes(".");
  }
  if (filter.endsWith(".>")) {
    const prefix = filter.slice(0, -1);
    return subject.startsWith(prefix);
  }
  return false;
}

export class HubEngine {
  readonly db: DatabaseSync;
  readonly events = new EventEmitter();
  readonly authMode: HubAuthMode;
  private readonly now: () => number;
  private readonly auditLogPath?: string;
  private readonly onAudit?: (event: HubAuditEvent) => void;
  private readonly env: NodeJS.ProcessEnv;
  readonly fetchFn?: typeof fetch;
  private readonly consumers = new Map<string, ConsumerState>();
  private readonly runningPluginServices = new Map<string, { stop(): Promise<void> | void }>();
  private closed = false;

  constructor(options: HubEngineOptions = {}) {
    this.authMode = options.authMode ?? "none";
    this.now = options.now ?? Date.now;
    this.auditLogPath = options.auditLogPath ?? process.env.CITO_HUB_AUDIT_LOG?.trim() ?? undefined;
    this.onAudit = options.onAudit;
    this.env = options.env ?? process.env;
    this.fetchFn = options.fetchFn;
    this.events.setMaxListeners(0);

    if (options.dataDir) {
      mkdirSync(options.dataDir, { recursive: true, mode: 0o700 });
      chmodSync(options.dataDir, 0o700);
      const dbPath = join(options.dataDir, "hub.sqlite");
      this.db = new DatabaseSync(dbPath);
      try { chmodSync(dbPath, 0o600); } catch {}
      this.db.exec("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;");
    } else {
      this.db = new DatabaseSync(":memory:");
    }

    this.db.exec(`
      CREATE TABLE IF NOT EXISTS hub_meta (
        key TEXT PRIMARY KEY,
        int_val INTEGER NOT NULL DEFAULT 0,
        json_val TEXT
      );
      INSERT OR IGNORE INTO hub_meta (key, int_val) VALUES ('kv_seq', 0);
      INSERT OR IGNORE INTO hub_meta (key, int_val, json_val) VALUES ('stream:CITO_SIGNALS', 0, '${JSON.stringify(DEFAULT_SIGNALS_STREAM_CONFIG)}');
      INSERT OR IGNORE INTO hub_meta (key, int_val, json_val) VALUES ('stream:KV_cito_listeners', 0, '${JSON.stringify(DEFAULT_KV_STREAM_CONFIG)}');

      CREATE TABLE IF NOT EXISTS hub_kv (
        key TEXT PRIMARY KEY,
        revision INTEGER NOT NULL,
        created_ms INTEGER NOT NULL,
        value BLOB NOT NULL,
        principal_id TEXT,
        scope TEXT
      );

      CREATE TABLE IF NOT EXISTS hub_signals (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        subject TEXT NOT NULL,
        msg_id TEXT UNIQUE,
        signal_id TEXT,
        created_ms INTEGER NOT NULL,
        data BLOB NOT NULL,
        acked INTEGER NOT NULL DEFAULT 0,
        claimed_by_channel TEXT,
        claim_until_ms INTEGER,
        acked_at_ms INTEGER
      );
      CREATE INDEX IF NOT EXISTS idx_hub_signals_subject_seq ON hub_signals(subject, seq);

      CREATE TABLE IF NOT EXISTS hub_plugin_services (
        plugin_id TEXT PRIMARY KEY,
        config_json TEXT NOT NULL DEFAULT '{}',
        service_state_json TEXT NOT NULL DEFAULT '{}',
        updated_ms INTEGER NOT NULL DEFAULT 0
      );

      CREATE TABLE IF NOT EXISTS hub_human_channels (
        principal_id TEXT NOT NULL,
        plugin_id TEXT NOT NULL,
        owner_user TEXT NOT NULL DEFAULT 'local',
        enabled INTEGER NOT NULL DEFAULT 1,
        user_config_json TEXT NOT NULL DEFAULT '{}',
        display_label TEXT,
        last_delivered_seq INTEGER NOT NULL DEFAULT 0,
        updated_ms INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (principal_id, plugin_id)
      );

      CREATE TABLE IF NOT EXISTS hub_channel_deliveries (
        plugin_id TEXT NOT NULL,
        principal_id TEXT NOT NULL,
        signal_id TEXT NOT NULL,
        seq INTEGER NOT NULL,
        status TEXT NOT NULL,
        external_msg_id TEXT,
        from_listener_id TEXT,
        from_user TEXT,
        from_title TEXT,
        delivered_ms INTEGER NOT NULL,
        PRIMARY KEY (plugin_id, principal_id, signal_id)
      );
      CREATE INDEX IF NOT EXISTS idx_hub_channel_deliveries_ext_msg
        ON hub_channel_deliveries(plugin_id, principal_id, external_msg_id);
      CREATE INDEX IF NOT EXISTS idx_hub_channel_deliveries_recent
        ON hub_channel_deliveries(plugin_id, principal_id, delivered_ms DESC);

      CREATE TABLE IF NOT EXISTS hub_pairing_codes (
        code TEXT PRIMARY KEY,
        plugin_id TEXT NOT NULL,
        principal_id TEXT NOT NULL,
        owner_user TEXT NOT NULL,
        created_ms INTEGER NOT NULL,
        expires_ms INTEGER NOT NULL,
        consumed_ms INTEGER
      );

      CREATE TABLE IF NOT EXISTS hub_audit_log (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        ts_ms INTEGER NOT NULL,
        principal_id TEXT NOT NULL,
        action TEXT NOT NULL,
        target_id TEXT NOT NULL,
        detail_json TEXT
      );
    `);

    for (const ddl of [
      "ALTER TABLE hub_signals ADD COLUMN signal_id TEXT",
      "ALTER TABLE hub_signals ADD COLUMN claimed_by_channel TEXT",
      "ALTER TABLE hub_signals ADD COLUMN claim_until_ms INTEGER",
      "ALTER TABLE hub_signals ADD COLUMN acked_at_ms INTEGER",
      "CREATE INDEX IF NOT EXISTS idx_hub_signals_signal_id ON hub_signals(signal_id)",
    ]) {
      try { this.db.exec(ddl); } catch {}
    }

    this.seedPluginServicesFromEnv();
    this.syncPluginBackgroundServices();
    void this.replayPendingHumanDeliveries();
  }

  private seedPluginServicesFromEnv(): void {
    for (const plugin of getHumanChannelPlugins()) {
      const fields = plugin.manifest?.fields ?? [];
      const hubFields = fields.filter(f => f.scope === "hub");
      if (hubFields.length === 0) continue;
      const envConfig: Record<string, unknown> = {};
      for (const f of hubFields) {
        const snakeKey = f.key.replace(/([a-z0-9])([A-Z])/g, "$1_$2").toUpperCase();
        const raw =
          this.env[`CITO_PLUGIN_${plugin.id.toUpperCase()}_${snakeKey}`] ??
          this.env[`CITO_PLUGIN_${plugin.id.toUpperCase()}_${f.key.toUpperCase()}`];
        if (typeof raw === "string" && raw.trim()) {
          envConfig[f.key] = raw.trim();
        }
      }
      if (Object.keys(envConfig).length > 0) {
        const existing = this.getPluginServiceRow(plugin.id);
        const merged = { ...envConfig, ...existing.config };
        this.savePluginServiceConfig(plugin.id, merged);
      }
    }
  }

  private getPluginServiceRow(pluginId: string): {
    config: Record<string, unknown>;
    serviceState: Record<string, unknown>;
  } {
    const row = this.db
      .prepare("SELECT config_json, service_state_json FROM hub_plugin_services WHERE plugin_id = ?")
      .get(pluginId) as { config_json?: string; service_state_json?: string } | undefined;
    let config: Record<string, unknown> = {};
    let serviceState: Record<string, unknown> = {};
    if (row?.config_json) {
      try { config = JSON.parse(row.config_json) as Record<string, unknown>; } catch {}
    }
    if (row?.service_state_json) {
      try { serviceState = JSON.parse(row.service_state_json) as Record<string, unknown>; } catch {}
    }
    return { config, serviceState };
  }

  private savePluginServiceConfig(pluginId: string, config: Record<string, unknown>): void {
    const now = this.now();
    this.db
      .prepare(`
        INSERT INTO hub_plugin_services (plugin_id, config_json, service_state_json, updated_ms)
        VALUES (?, ?, '{}', ?)
        ON CONFLICT(plugin_id) DO UPDATE SET config_json = excluded.config_json, updated_ms = excluded.updated_ms
      `)
      .run(pluginId, JSON.stringify(config), now);
  }

  private savePluginServiceState(pluginId: string, state: Record<string, unknown>): void {
    const now = this.now();
    this.db
      .prepare(`
        INSERT INTO hub_plugin_services (plugin_id, config_json, service_state_json, updated_ms)
        VALUES (?, '{}', ?, ?)
        ON CONFLICT(plugin_id) DO UPDATE SET service_state_json = excluded.service_state_json, updated_ms = excluded.updated_ms
      `)
      .run(pluginId, JSON.stringify(state), now);
  }

  private createServiceContext(plugin: HumanChannelPlugin): HumanChannelServiceContext {
    return {
      fetchFn: this.fetchFn,
      getHubConfig: () => this.getPluginServiceRow(plugin.id).config,
      getServiceState: () => this.getPluginServiceRow(plugin.id).serviceState,
      setServiceState: state => {
        if (this.closed) return;
        this.savePluginServiceState(plugin.id, state);
      },
      completePairing: (code, result) => {
        if (this.closed) return { ok: false, error: "Hub is closed" };
        const cleanCode = code.trim().toLowerCase();
        const now = this.now();
        const row = this.db
          .prepare("SELECT * FROM hub_pairing_codes WHERE code = ? AND plugin_id = ?")
          .get(cleanCode, plugin.id) as {
            code: string;
            plugin_id: string;
            principal_id: string;
            owner_user: string;
            expires_ms: number | bigint;
            consumed_ms: number | bigint | null;
          } | undefined;
        if (!row) {
          return { ok: false, error: "Invalid pairing code" };
        }
        if (row.consumed_ms !== null) {
          return { ok: false, error: "Pairing code was already used" };
        }
        if (Number(row.expires_ms) < now) {
          return { ok: false, error: "Pairing code has expired" };
        }
        this.transaction(() => {
          this.db
            .prepare("UPDATE hub_pairing_codes SET consumed_ms = ? WHERE code = ?")
            .run(now, cleanCode);
          this.upsertHumanChannelBinding({
            principalId: row.principal_id,
            ownerUser: row.owner_user,
            pluginId: plugin.id,
            enabled: true,
            userConfig: result.userConfig,
            displayLabel: result.displayLabel ?? null,
          });
        });
        this.ensureHumanChannelNodeAttachment(
          { principal_id: row.principal_id, owner_user: row.owner_user },
          plugin,
          true,
        );
        this.events.emit("human_channel_updated", {
          plugin_id: plugin.id,
          principal_id: row.principal_id,
          owner_user: row.owner_user,
        });
        return {
          ok: true,
          principal_id: row.principal_id,
          owner_user: row.owner_user,
        };
      },
      resolvePrincipalByUserConfig: (matchKey, matchValue) => {
        const rows = this.db
          .prepare("SELECT principal_id, owner_user, user_config_json FROM hub_human_channels WHERE plugin_id = ? AND enabled = 1")
          .all(plugin.id) as {
            principal_id: string;
            owner_user: string;
            user_config_json: string;
          }[];
        for (const r of rows) {
          try {
            const cfg = JSON.parse(r.user_config_json) as Record<string, unknown>;
            if (String(cfg[matchKey] ?? "") === matchValue) {
              return { principal_id: r.principal_id, owner_user: r.owner_user, userConfig: cfg };
            }
          } catch {}
        }
        return undefined;
      },
      resolveReplyTarget: (principalId, options) => {
        if (options.replyToExternalMsgId) {
          const row = this.db
            .prepare(`
              SELECT signal_id, from_listener_id, from_user, from_title
              FROM hub_channel_deliveries
              WHERE plugin_id = ? AND principal_id = ? AND external_msg_id = ?
              ORDER BY delivered_ms DESC LIMIT 1
            `)
            .get(plugin.id, principalId, options.replyToExternalMsgId) as {
              signal_id: string;
              from_listener_id: string | null;
              from_user: string | null;
              from_title: string | null;
            } | undefined;
          if (row && row.from_listener_id) {
            return {
              signal_id: row.signal_id,
              from_listener_id: row.from_listener_id,
              ...(row.from_user ? { from_user: row.from_user } : {}),
              ...(row.from_title ? { from_title: row.from_title } : {}),
            };
          }
          return undefined;
        }
        const cutoff = this.now() - (options.maxAgeMs ?? 10 * 60_000);
        const rows = this.db
          .prepare(`
            SELECT d.signal_id, d.from_listener_id, d.from_user, d.from_title, s.acked_at_ms
            FROM hub_channel_deliveries d
            LEFT JOIN hub_signals s ON s.seq = d.seq
            WHERE d.plugin_id = ? AND d.principal_id = ? AND d.delivered_ms >= ? AND d.from_listener_id IS NOT NULL
            ORDER BY (s.acked_at_ms IS NULL) DESC, d.delivered_ms DESC
            LIMIT 1
          `)
          .all(plugin.id, principalId, cutoff) as {
            signal_id: string;
            from_listener_id: string;
            from_user: string | null;
            from_title: string | null;
            acked_at_ms: number | bigint | null;
          }[];
        const match = rows[0];
        if (!match) return undefined;
        return {
          signal_id: match.signal_id,
          from_listener_id: match.from_listener_id,
          ...(match.from_user ? { from_user: match.from_user } : {}),
          ...(match.from_title ? { from_title: match.from_title } : {}),
        };
      },
      sendReplyFromHuman: async (principalId, options) => {
        const chRow = this.db
          .prepare("SELECT owner_user FROM hub_human_channels WHERE principal_id = ? AND plugin_id = ?")
          .get(principalId, plugin.id) as { owner_user?: string } | undefined;
        const ownerUser = chRow?.owner_user ?? principalId.split("#").at(-1) ?? "user";
        const caller: Principal = { principal_id: principalId, owner_user: ownerUser };
        let canonicalTo: string;
        try {
          canonicalTo = toCanonicalListenerId(options.to_listener_id);
        } catch {
          return { ok: false, error: `Invalid recipient ID: ${options.to_listener_id}` };
        }
        const targetEntry = this.db
          .prepare("SELECT value FROM hub_kv WHERE key = ?")
          .get(canonicalTo) as { value?: Uint8Array } | undefined;
        let toLabel = options.to_listener_id;
        let targetGeneration = 1;
        if (targetEntry?.value) {
          try {
            const parsed = JSON.parse(Buffer.from(targetEntry.value).toString("utf8")) as {
              title?: string;
              public_id?: string;
              binding_generation?: number;
            };
            if (typeof parsed.binding_generation === "number") targetGeneration = parsed.binding_generation;
            if (parsed.title) toLabel = parsed.title;
            else if (parsed.public_id) toLabel = parsed.public_id;
          } catch {}
        }

        const humanNodeId = listenerId(humanNodeKey(caller));
        const humanPubId = publicListenerId(ownerUser, humanNodeId);
        const signalId = "sig_" + randomUUID().replaceAll("-", "");
        const wirePayload = {
          signal_id: signalId,
          to: canonicalTo,
          target_generation: targetGeneration,
          sender_endpoint_key: `${principalId}|hub|human|default`,
          from: {
            principal_id: principalId,
            user: ownerUser,
            host_id: "hub",
            host_name: plugin.manifest?.displayName ?? plugin.id,
            harness: "human",
            listener_id: humanPubId,
            title: `@${ownerUser} (Me)`,
            about: `${plugin.manifest?.displayName ?? plugin.id} channel`,
          },
          message: options.message,
          in_reply_to: options.in_reply_to ?? null,
        };
        this.jsPublish(
          "cito.signal." + canonicalTo,
          Buffer.from(JSON.stringify(wirePayload), "utf8"),
          { msgID: signalId },
          caller,
        );
        if (options.in_reply_to) {
          this.signalAck(
            {
              node_id: humanNodeId,
              signal_ids: [options.in_reply_to],
              channel_id: `hub:${plugin.id}`,
            },
            caller,
          );
        }
        return { ok: true, signal_id: signalId, to_label: toLabel };
      },
    };
  }

  private syncPluginBackgroundServices(): void {
    if (this.closed) return;
    for (const plugin of getHumanChannelPlugins()) {
      if (plugin.id === "human" || !plugin.startService) continue;
      const { config } = this.getPluginServiceRow(plugin.id);
      const hubFields = (plugin.manifest?.fields ?? []).filter(f => f.scope === "hub" && f.required);
      const ready = hubFields.every(f => typeof config[f.key] === "string" && String(config[f.key]).trim().length > 0);
      if (ready && !this.runningPluginServices.has(plugin.id)) {
        const ctx = this.createServiceContext(plugin);
        void Promise.resolve(plugin.startService(ctx)).then(handle => {
          if (handle && !this.closed) {
            this.runningPluginServices.set(plugin.id, handle);
          } else if (handle && this.closed) {
            void handle.stop();
          }
        }).catch(() => {});
      }
    }
  }

  async pollHumanChannel(pluginId: string): Promise<{ processed: number }> {
    const plugin = getHumanChannelPlugin(pluginId);
    if (!plugin || !plugin.pollOnce) return { processed: 0 };
    const ctx = this.createServiceContext(plugin);
    const processed = await plugin.pollOnce(ctx);
    return { processed };
  }

  private upsertHumanChannelBinding(params: {
    principalId: string;
    ownerUser: string;
    pluginId: string;
    enabled: boolean;
    userConfig: Record<string, unknown>;
    displayLabel?: string | null;
  }): void {
    const now = this.now();
    const existing = this.db
      .prepare("SELECT user_config_json, display_label, last_delivered_seq FROM hub_human_channels WHERE principal_id = ? AND plugin_id = ?")
      .get(params.principalId, params.pluginId) as {
        user_config_json: string;
        display_label: string | null;
        last_delivered_seq: number | bigint;
      } | undefined;
    let mergedUserConfig: Record<string, unknown> = {};
    if (existing?.user_config_json) {
      try { mergedUserConfig = JSON.parse(existing.user_config_json) as Record<string, unknown>; } catch {}
    }
    for (const [k, v] of Object.entries(params.userConfig)) {
      if (v !== undefined) mergedUserConfig[k] = v;
    }
    const maxSeqRow = this.db.prepare("SELECT COALESCE(MAX(seq), 0) AS max_seq FROM hub_signals").get() as { max_seq: number | bigint };
    const initialLastDeliveredSeq = existing ? Number(existing.last_delivered_seq) : Number(maxSeqRow.max_seq);
    const displayLabel = params.displayLabel !== undefined ? params.displayLabel : (existing?.display_label ?? null);
    this.db
      .prepare(`
        INSERT INTO hub_human_channels (principal_id, plugin_id, owner_user, enabled, user_config_json, display_label, last_delivered_seq, updated_ms)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(principal_id, plugin_id) DO UPDATE SET
          owner_user = excluded.owner_user,
          enabled = excluded.enabled,
          user_config_json = excluded.user_config_json,
          display_label = excluded.display_label,
          updated_ms = excluded.updated_ms
      `)
      .run(
        params.principalId,
        params.pluginId,
        params.ownerUser,
        params.enabled ? 1 : 0,
        JSON.stringify(mergedUserConfig),
        displayLabel,
        initialLastDeliveredSeq,
        now,
      );
  }

  private ensureHumanChannelNodeAttachment(
    principal: Principal,
    plugin: HumanChannelPlugin,
    active: boolean,
  ): void {
    const nodeId = listenerId(humanNodeKey(principal));
    const pubId = publicListenerId(principal.owner_user, nodeId);
    const channelId = `hub:${plugin.id}`;
    const now = this.now();
    const existingRow = this.db.prepare("SELECT revision, value FROM hub_kv WHERE key = ?").get(nodeId) as {
      revision: number | bigint;
      value: Uint8Array;
    } | undefined;
    let currentObj: Record<string, unknown> | undefined;
    if (existingRow?.value) {
      try {
        currentObj = JSON.parse(Buffer.from(existingRow.value).toString("utf8")) as Record<string, unknown>;
      } catch {}
    }
    const existingChannels = (currentObj?.channels && typeof currentObj.channels === "object" && !Array.isArray(currentObj.channels))
      ? { ...(currentObj.channels as Record<string, ChannelAttachment>) }
      : {};
    existingChannels[channelId] = {
      channel_id: channelId,
      kind: plugin.id,
      host_id: "hub",
      host_name: plugin.manifest?.displayName ?? plugin.id,
      delivery_mode: "notify_fanout",
      priority: 50,
      state: active ? "ACTIVE" : "UNLISTENED",
      updated_at_ms: now,
    };
    const anyActive = Object.values(existingChannels).some(c => c.state === "ACTIVE");
    const defaultScope = this.authMode === "oidc" ? "self" : "workspace";
    const nextReg = {
      listener_id: nodeId,
      public_id: typeof currentObj?.public_id === "string" ? currentObj.public_id : pubId,
      tag: (currentObj?.tag as string | null | undefined) ?? principal.owner_user,
      principal_id: principal.principal_id,
      owner_user: principal.owner_user,
      host_id: typeof currentObj?.host_id === "string" ? currentObj.host_id : "hub",
      host_name: typeof currentObj?.host_name === "string" ? currentObj.host_name : (plugin.manifest?.displayName ?? plugin.id),
      harness: "human",
      session_id: "default",
      cwd: typeof currentObj?.cwd === "string" ? currentObj.cwd : "~",
      repo: null,
      branch: null,
      title: typeof currentObj?.title === "string" ? currentObj.title : `@${principal.owner_user} (Me)`,
      about: typeof currentObj?.about === "string" ? currentObj.about : "Human notifications",
      scope: (currentObj?.scope === "self" || currentObj?.scope === "workspace") ? currentObj.scope : defaultScope,
      state: anyActive ? "ACTIVE" : "UNLISTENED",
      binding_generation: typeof currentObj?.binding_generation === "number" ? currentObj.binding_generation : 1,
      owner_token: typeof currentObj?.owner_token === "string" ? currentObj.owner_token : randomUUID(),
      channels: existingChannels,
    };
    this.kvPut(nodeId, Buffer.from(JSON.stringify(nextReg), "utf8"), principal);
  }

  private async deliverSignalToHumanChannels(
    seq: number,
    targetNodeId: string,
    createdMs: number,
    rawSignal: Record<string, unknown>,
  ): Promise<void> {
    if (this.closed) return;
    let principalId: string | null = null;
    let ownerUser = "local";
    const kvRow = this.db.prepare("SELECT value, principal_id FROM hub_kv WHERE key = ?").get(targetNodeId) as {
      value?: Uint8Array;
      principal_id?: string | null;
    } | undefined;
    if (kvRow?.value) {
      try {
        const parsed = JSON.parse(Buffer.from(kvRow.value).toString("utf8")) as {
          harness?: string;
          principal_id?: string;
          owner_user?: string;
        };
        if (parsed.harness && parsed.harness !== "human") return;
        principalId = parsed.principal_id ?? kvRow.principal_id ?? null;
        if (parsed.owner_user) ownerUser = parsed.owner_user;
      } catch {}
    }
    if (!principalId) {
      const chRows = this.db.prepare("SELECT DISTINCT principal_id, owner_user FROM hub_human_channels WHERE enabled = 1").all() as {
        principal_id: string;
        owner_user: string;
      }[];
      for (const r of chRows) {
        if (listenerId(humanNodeKey(r)) === targetNodeId) {
          principalId = r.principal_id;
          ownerUser = r.owner_user;
          break;
        }
      }
    }
    if (!principalId) return;

    const signalId = typeof rawSignal.signal_id === "string" ? rawSignal.signal_id : `sig_${seq}`;
    const fromObj = (rawSignal.from && typeof rawSignal.from === "object" && !Array.isArray(rawSignal.from))
      ? (rawSignal.from as Record<string, unknown>)
      : {};
    const notification: HumanNotificationPayload = {
      signal_id: signalId,
      seq,
      principal_id: principalId,
      owner_user: ownerUser,
      from_user: typeof fromObj.user === "string" ? fromObj.user : "agent",
      from_host_name: typeof fromObj.host_name === "string" ? fromObj.host_name : "host",
      from_harness: typeof fromObj.harness === "string" ? fromObj.harness : null,
      from_listener_id: typeof fromObj.listener_id === "string" ? fromObj.listener_id : null,
      from_title: typeof fromObj.title === "string" ? fromObj.title : "",
      from_about: typeof fromObj.about === "string" ? fromObj.about : null,
      message: typeof rawSignal.message === "string" ? rawSignal.message : "",
      in_reply_to: typeof rawSignal.in_reply_to === "string" ? rawSignal.in_reply_to : null,
      created_at: new Date(createdMs).toISOString(),
    };

    const bindings = this.db
      .prepare("SELECT plugin_id, user_config_json, last_delivered_seq FROM hub_human_channels WHERE principal_id = ? AND enabled = 1")
      .all(principalId) as {
        plugin_id: string;
        user_config_json: string;
        last_delivered_seq: number | bigint;
      }[];

    for (const binding of bindings) {
      if (binding.plugin_id === "human") continue;
      if (seq <= Number(binding.last_delivered_seq)) continue;
      const already = this.db
        .prepare("SELECT status FROM hub_channel_deliveries WHERE plugin_id = ? AND principal_id = ? AND signal_id = ?")
        .get(binding.plugin_id, principalId, signalId) as { status?: string } | undefined;
      if (already?.status === "delivered") continue;

      const plugin = getHumanChannelPlugin(binding.plugin_id);
      if (!plugin) continue;
      const { config: hubConfig } = this.getPluginServiceRow(plugin.id);
      let userConfig: Record<string, unknown> = {};
      try { userConfig = JSON.parse(binding.user_config_json) as Record<string, unknown>; } catch {}

      try {
        const res = await plugin.sendNotification(notification, {
          hubConfig,
          userConfig,
          fetchFn: this.fetchFn,
        });
        if (this.closed) return;
        if (res.delivered) {
          const now = this.now();
          this.transaction(() => {
            this.db
              .prepare(`
                INSERT OR REPLACE INTO hub_channel_deliveries (
                  plugin_id, principal_id, signal_id, seq, status, external_msg_id, from_listener_id, from_user, from_title, delivered_ms
                ) VALUES (?, ?, ?, ?, 'delivered', ?, ?, ?, ?, ?)
              `)
              .run(
                plugin.id,
                principalId,
                signalId,
                seq,
                res.externalMessageId ?? null,
                notification.from_listener_id,
                notification.from_user,
                notification.from_title,
                now,
              );
            this.db
              .prepare("UPDATE hub_human_channels SET last_delivered_seq = MAX(last_delivered_seq, ?) WHERE principal_id = ? AND plugin_id = ?")
              .run(seq, principalId, plugin.id);
          });
        }
      } catch {}
    }
  }

  async replayPendingHumanDeliveries(): Promise<void> {
    if (this.closed) return;
    const bindings = this.db
      .prepare("SELECT DISTINCT principal_id, owner_user, MIN(last_delivered_seq) AS min_seq FROM hub_human_channels WHERE enabled = 1 GROUP BY principal_id")
      .all() as { principal_id: string; owner_user: string; min_seq: number | bigint }[];
    for (const b of bindings) {
      const nodeId = listenerId(humanNodeKey(b));
      const subject = "cito.signal." + nodeId;
      const rows = this.db
        .prepare("SELECT seq, created_ms, data FROM hub_signals WHERE subject = ? AND seq > ? AND acked_at_ms IS NULL ORDER BY seq ASC")
        .all(subject, Number(b.min_seq)) as { seq: number | bigint; created_ms: number | bigint; data: Uint8Array }[];
      for (const r of rows) {
        try {
          const parsed = JSON.parse(Buffer.from(r.data).toString("utf8")) as Record<string, unknown>;
          await this.deliverSignalToHumanChannels(Number(r.seq), nodeId, Number(r.created_ms), parsed);
        } catch {}
      }
    }
  }

  private transaction<T>(fn: () => T): T {
    this.db.exec("BEGIN IMMEDIATE");
    try {
      const result = fn();
      this.db.exec("COMMIT");
      return result;
    } catch (error) {
      this.db.exec("ROLLBACK");
      throw error;
    }
  }

  private nextKvSeq(): number {
    this.db.prepare("UPDATE hub_meta SET int_val = int_val + 1 WHERE key = 'kv_seq'").run();
    const row = this.db.prepare("SELECT int_val FROM hub_meta WHERE key = 'kv_seq'").get() as { int_val: number | bigint };
    return Number(row.int_val);
  }

  private recordAudit(
    principalId: string,
    action: "register" | "unlisten" | "send" | "ack",
    targetId: string,
    detail?: Record<string, unknown>,
  ): void {
    const tsMs = this.now();
    const detailJson = detail ? JSON.stringify(detail) : null;
    this.db.prepare(
      "INSERT INTO hub_audit_log (ts_ms, principal_id, action, target_id, detail_json) VALUES (?, ?, ?, ?, ?)",
    ).run(tsMs, principalId, action, targetId, detailJson);
    const event: HubAuditEvent = {
      ts_ms: tsMs,
      principal_id: principalId,
      action,
      target_id: targetId,
      ...(typeof detail?.listener_id === "string" ? { listener_id: detail.listener_id } : {}),
      ...(typeof detail?.signal_id === "string" ? { signal_id: detail.signal_id } : {}),
      ...(detail ? { detail } : {}),
    };
    if (this.auditLogPath) {
      try {
        mkdirSync(dirname(this.auditLogPath), { recursive: true });
        appendFileSync(this.auditLogPath, JSON.stringify(event) + "\n", { mode: 0o600 });
      } catch {}
    }
    this.onAudit?.(event);
  }

  private parseKvValue(valueBytes: Uint8Array): {
    principalId: string | null;
    scope: string | null;
    state: "ACTIVE" | "UNLISTENED" | null;
    channels?: Record<string, ChannelAttachment>;
  } {
    try {
      const parsed: unknown = JSON.parse(Buffer.from(valueBytes).toString("utf8"));
      if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
        const obj = parsed as Record<string, unknown>;
        return {
          principalId: typeof obj.principal_id === "string" ? obj.principal_id : null,
          scope: typeof obj.scope === "string" ? obj.scope : null,
          state: obj.state === "ACTIVE" ? "ACTIVE" : obj.state === "UNLISTENED" ? "UNLISTENED" : null,
          channels: (obj.channels && typeof obj.channels === "object" && !Array.isArray(obj.channels))
            ? (obj.channels as Record<string, ChannelAttachment>)
            : undefined,
        };
      }
    } catch {}
    return { principalId: null, scope: null, state: null };
  }

  canReadKvRow(row: { principal_id: string | null; scope: string | null }, caller: Principal | null): boolean {
    if (this.authMode !== "oidc") return true;
    if (row.scope === "self" && row.principal_id && row.principal_id !== caller?.principal_id) {
      return false;
    }
    return true;
  }

  canConsumeSignal(targetId: string, caller: Principal | null): boolean {
    if (this.authMode !== "oidc") return true;
    const row = this.db.prepare("SELECT principal_id FROM hub_kv WHERE key = ?").get(targetId) as { principal_id: string | null } | undefined;
    if (row && row.principal_id && row.principal_id !== caller?.principal_id) {
      return false;
    }
    return true;
  }

  streamInfo(stream: string): {
    config: StreamConfig;
    state: { messages: number; last_seq: number; first_seq: number; bytes: number; consumer_count: number };
    ts: string;
  } {
    const meta = this.db.prepare("SELECT json_val FROM hub_meta WHERE key = ?").get("stream:" + stream) as { json_val?: string } | undefined;
    if (!meta?.json_val) {
      throw new HubError(404, "10059", `stream not found: ${stream}`, 10059);
    }
    const config = JSON.parse(meta.json_val) as StreamConfig;
    const ts = new Date(this.now()).toISOString();
    if (stream === KV_LISTENERS_STREAM) {
      const countRow = this.db.prepare("SELECT COUNT(*) AS n FROM hub_kv").get() as { n: number | bigint };
      const seqRow = this.db.prepare("SELECT int_val FROM hub_meta WHERE key = 'kv_seq'").get() as { int_val: number | bigint };
      const messages = Number(countRow.n);
      const lastSeq = Number(seqRow.int_val);
      return {
        config,
        state: {
          messages,
          last_seq: lastSeq,
          first_seq: messages > 0 ? 1 : 0,
          bytes: 0,
          consumer_count: 0,
        },
        ts,
      };
    }
    if (stream === SIGNALS_STREAM) {
      const countRow = this.db.prepare("SELECT COUNT(*) AS n, COALESCE(MAX(seq), 0) AS max_seq, COALESCE(MIN(seq), 0) AS min_seq FROM hub_signals").get() as {
        n: number | bigint;
        max_seq: number | bigint;
        min_seq: number | bigint;
      };
      const messages = Number(countRow.n);
      return {
        config,
        state: {
          messages,
          last_seq: Number(countRow.max_seq),
          first_seq: Number(countRow.min_seq),
          bytes: 0,
          consumer_count: this.consumers.size,
        },
        ts,
      };
    }
    return {
      config,
      state: { messages: 0, last_seq: 0, first_seq: 0, bytes: 0, consumer_count: 0 },
      ts,
    };
  }

  streamAdd(config: Partial<StreamConfig> & { name: string }): ReturnType<HubEngine["streamInfo"]> {
    const existing = this.db.prepare("SELECT json_val FROM hub_meta WHERE key = ?").get("stream:" + config.name) as { json_val?: string } | undefined;
    if (!existing?.json_val) {
      const merged: StreamConfig = {
        ...DEFAULT_SIGNALS_STREAM_CONFIG,
        ...config,
      };
      this.db.prepare("INSERT OR REPLACE INTO hub_meta (key, int_val, json_val) VALUES (?, 0, ?)").run(
        "stream:" + config.name,
        JSON.stringify(merged),
      );
    }
    return this.streamInfo(config.name);
  }

  streamUpdate(stream: string, patch: Partial<StreamConfig>): ReturnType<HubEngine["streamInfo"]> {
    const info = this.streamInfo(stream);
    const updated: StreamConfig = { ...info.config, ...patch, name: stream };
    this.db.prepare("UPDATE hub_meta SET json_val = ? WHERE key = ?").run(JSON.stringify(updated), "stream:" + stream);
    return this.streamInfo(stream);
  }

  streamGetMessage(
    stream: string,
    seq: number,
    _caller: Principal | null,
  ): { seq: number; subject: string; created_ms: number; data_b64: string } {
    if (stream !== SIGNALS_STREAM) {
      throw new HubError(404, "10037", "no message found", 10037);
    }
    const row = this.db.prepare("SELECT seq, subject, created_ms, data FROM hub_signals WHERE seq = ?").get(seq) as {
      seq: number | bigint;
      subject: string;
      created_ms: number | bigint;
      data: Uint8Array;
    } | undefined;
    if (!row) {
      throw new HubError(404, "10037", "no message found", 10037);
    }
    return {
      seq: Number(row.seq),
      subject: row.subject,
      created_ms: Number(row.created_ms),
      data_b64: Buffer.from(row.data).toString("base64"),
    };
  }

  kvGet(
    key: string,
    caller: Principal | null,
  ): { key: string; revision: number; created_ms: number; value_b64: string; operation: "PUT" } | { forbidden_scope: true } | null {
    const row = this.db.prepare(
      "SELECT key, revision, created_ms, value, principal_id, scope FROM hub_kv WHERE key = ?",
    ).get(key) as {
      key: string;
      revision: number | bigint;
      created_ms: number | bigint;
      value: Uint8Array;
      principal_id: string | null;
      scope: string | null;
    } | undefined;
    if (!row) return null;
    if (!this.canReadKvRow(row, caller)) {
      const parsed = this.parseKvValue(row.value);
      const lease = evaluateNodeLease(
        { state: parsed.state, channels: parsed.channels },
        this.now(),
        Number(row.created_ms),
      );
      if (lease === "ACTIVE" || lease === "DISCONNECTED_RETAINED") {
        return { forbidden_scope: true };
      }
      return null;
    }
    return {
      key: row.key,
      revision: Number(row.revision),
      created_ms: Number(row.created_ms),
      value_b64: Buffer.from(row.value).toString("base64"),
      operation: "PUT",
    };
  }

  private assertKvWriteAllowed(
    existing: { principal_id: string | null } | undefined,
    incomingPrincipalId: string | null,
    caller: Principal | null,
  ): string | null {
    if (this.authMode === "oidc") {
      if (!caller) {
        throw new HubError(401, "AUTHORIZATION_VIOLATION", "Authorization Violation");
      }
      if (incomingPrincipalId !== null && incomingPrincipalId !== caller.principal_id) {
        throw new HubError(403, "PERMISSIONS_VIOLATION", "PERMISSIONS_VIOLATION: cannot write registration for another principal");
      }
      if (existing?.principal_id && existing.principal_id !== caller.principal_id) {
        throw new HubError(403, "PERMISSIONS_VIOLATION", "PERMISSIONS_VIOLATION: cannot overwrite registration owned by another principal");
      }
      return incomingPrincipalId ?? caller.principal_id;
    }
    return incomingPrincipalId ?? caller?.principal_id ?? null;
  }

  kvCreate(key: string, valueBytes: Uint8Array, caller: Principal | null): { revision: number } {
    const { principalId, scope, state } = this.parseKvValue(valueBytes);
    let revision = 0;
    let createdMs = 0;
    let storedPrincipal: string | null = null;
    const buf = Buffer.from(valueBytes);
    this.transaction(() => {
      const existing = this.db.prepare("SELECT revision, principal_id FROM hub_kv WHERE key = ?").get(key) as {
        revision: number | bigint;
        principal_id: string | null;
      } | undefined;
      storedPrincipal = this.assertKvWriteAllowed(existing, principalId, caller);
      if (existing) {
        throw new HubError(409, "10071", "wrong last sequence", 10071);
      }
      revision = this.nextKvSeq();
      createdMs = this.now();
      this.db.prepare(
        "INSERT INTO hub_kv (key, revision, created_ms, value, principal_id, scope) VALUES (?, ?, ?, ?, ?, ?)",
      ).run(key, revision, createdMs, buf, storedPrincipal, scope);
      this.recordAudit(
        storedPrincipal ?? caller?.principal_id ?? "anonymous",
        state === "UNLISTENED" ? "unlisten" : "register",
        key,
        { listener_id: key, revision, scope },
      );
    });
    const ev: HubKvEvent = {
      key,
      revision,
      created_ms: createdMs,
      value: buf,
      operation: "PUT",
      principal_id: storedPrincipal,
      scope,
    };
    this.events.emit("kv", ev);
    return { revision };
  }

  kvUpdate(key: string, valueBytes: Uint8Array, expectedRevision: number, caller: Principal | null): { revision: number } {
    const { principalId, scope, state } = this.parseKvValue(valueBytes);
    let revision = 0;
    let createdMs = 0;
    let storedPrincipal: string | null = null;
    const buf = Buffer.from(valueBytes);
    this.transaction(() => {
      const existing = this.db.prepare("SELECT revision, principal_id FROM hub_kv WHERE key = ?").get(key) as {
        revision: number | bigint;
        principal_id: string | null;
      } | undefined;
      storedPrincipal = this.assertKvWriteAllowed(existing, principalId, caller);
      if (!existing || Number(existing.revision) !== Number(expectedRevision)) {
        throw new HubError(409, "10071", "wrong last sequence", 10071);
      }
      revision = this.nextKvSeq();
      createdMs = this.now();
      this.db.prepare(
        "UPDATE hub_kv SET revision = ?, created_ms = ?, value = ?, principal_id = ?, scope = ? WHERE key = ?",
      ).run(revision, createdMs, buf, storedPrincipal, scope, key);
      this.recordAudit(
        storedPrincipal ?? caller?.principal_id ?? "anonymous",
        state === "UNLISTENED" ? "unlisten" : "register",
        key,
        { listener_id: key, revision, scope },
      );
    });
    const ev: HubKvEvent = {
      key,
      revision,
      created_ms: createdMs,
      value: buf,
      operation: "PUT",
      principal_id: storedPrincipal,
      scope,
    };
    this.events.emit("kv", ev);
    return { revision };
  }

  kvPut(key: string, valueBytes: Uint8Array, caller: Principal | null): { revision: number } {
    const { principalId, scope, state } = this.parseKvValue(valueBytes);
    let revision = 0;
    let createdMs = 0;
    let storedPrincipal: string | null = null;
    const buf = Buffer.from(valueBytes);
    this.transaction(() => {
      const existing = this.db.prepare("SELECT revision, principal_id FROM hub_kv WHERE key = ?").get(key) as {
        revision: number | bigint;
        principal_id: string | null;
      } | undefined;
      storedPrincipal = this.assertKvWriteAllowed(existing, principalId, caller);
      revision = this.nextKvSeq();
      createdMs = this.now();
      this.db.prepare(
        "INSERT OR REPLACE INTO hub_kv (key, revision, created_ms, value, principal_id, scope) VALUES (?, ?, ?, ?, ?, ?)",
      ).run(key, revision, createdMs, buf, storedPrincipal, scope);
      this.recordAudit(
        storedPrincipal ?? caller?.principal_id ?? "anonymous",
        state === "UNLISTENED" ? "unlisten" : "register",
        key,
        { listener_id: key, revision, scope },
      );
    });
    const ev: HubKvEvent = {
      key,
      revision,
      created_ms: createdMs,
      value: buf,
      operation: "PUT",
      principal_id: storedPrincipal,
      scope,
    };
    this.events.emit("kv", ev);
    return { revision };
  }

  kvDelete(key: string, caller: Principal | null): { deleted: boolean } {
    let revision = 0;
    let createdMs = 0;
    this.transaction(() => {
      const existing = this.db.prepare("SELECT revision, principal_id FROM hub_kv WHERE key = ?").get(key) as {
        revision: number | bigint;
        principal_id: string | null;
      } | undefined;
      this.assertKvWriteAllowed(existing, null, caller);
      revision = this.nextKvSeq();
      createdMs = this.now();
      this.db.prepare("DELETE FROM hub_kv WHERE key = ?").run(key);
    });
    const ev: HubKvEvent = {
      key,
      revision,
      created_ms: createdMs,
      value: Buffer.alloc(0),
      operation: "DEL",
      principal_id: caller?.principal_id ?? null,
      scope: null,
    };
    this.events.emit("kv", ev);
    return { deleted: true };
  }

  kvKeys(caller: Principal | null): { keys: string[] } {
    const rows = this.db.prepare("SELECT key, principal_id, scope FROM hub_kv ORDER BY revision ASC").all() as {
      key: string;
      principal_id: string | null;
      scope: string | null;
    }[];
    return {
      keys: rows.filter(r => this.canReadKvRow(r, caller)).map(r => r.key),
    };
  }

  kvSnapshot(
    keyFilter: string | undefined,
    caller: Principal | null,
  ): {
    entries: { key: string; revision: number; created_ms: number; value_b64: string; operation: "PUT" }[];
  } {
    const rows = (
      keyFilter && !keyFilter.includes("*") && !keyFilter.includes(">")
        ? this.db.prepare("SELECT key, revision, created_ms, value, principal_id, scope FROM hub_kv WHERE key = ? ORDER BY revision ASC").all(keyFilter)
        : this.db.prepare("SELECT key, revision, created_ms, value, principal_id, scope FROM hub_kv ORDER BY revision ASC").all()
    ) as {
      key: string;
      revision: number | bigint;
      created_ms: number | bigint;
      value: Uint8Array;
      principal_id: string | null;
      scope: string | null;
    }[];
    const visible = rows.filter(r => this.canReadKvRow(r, caller));
    return {
      entries: visible.map(r => ({
        key: r.key,
        revision: Number(r.revision),
        created_ms: Number(r.created_ms),
        value_b64: Buffer.from(r.value).toString("base64"),
        operation: "PUT" as const,
      })),
    };
  }

  jsPublish(
    subject: string,
    dataBytes: Uint8Array,
    opts: { msgID?: string } | undefined,
    caller: Principal | null,
  ): { seq: number; duplicate: boolean; stream: string } {
    if (!isAllowedCitoSubject(subject)) {
      throw new HubError(403, "PERMISSIONS_VIOLATION", `PERMISSIONS_VIOLATION: subject '${subject}' is not permitted`);
    }
    if (subject.startsWith("cito.ack.")) {
      this.ncPublish(subject, dataBytes, caller);
      return { seq: 0, duplicate: false, stream: "" };
    }
    const targetId = subject.slice("cito.signal.".length);
    let parsedSignal: Record<string, unknown> & { signal_id?: unknown; from?: { principal_id?: unknown } } | undefined;
    try {
      const raw: unknown = JSON.parse(Buffer.from(dataBytes).toString("utf8"));
      if (raw && typeof raw === "object" && !Array.isArray(raw)) {
        parsedSignal = raw as Record<string, unknown> & { signal_id?: unknown; from?: { principal_id?: unknown } };
      }
    } catch {}

    if (this.authMode === "oidc") {
      if (!caller) {
        throw new HubError(401, "AUTHORIZATION_VIOLATION", "Authorization Violation");
      }
      if (parsedSignal && "from" in parsedSignal) {
        const fromPrincipal = parsedSignal.from?.principal_id;
        if (typeof fromPrincipal !== "string" || fromPrincipal !== caller.principal_id) {
          throw new HubError(403, "PERMISSIONS_VIOLATION", "PERMISSIONS_VIOLATION: signal sender principal mismatch");
        }
      }
      const targetRow = this.db.prepare("SELECT principal_id, scope FROM hub_kv WHERE key = ?").get(targetId) as {
        principal_id: string | null;
        scope: string | null;
      } | undefined;
      if (targetRow && targetRow.scope === "self" && targetRow.principal_id && targetRow.principal_id !== caller.principal_id) {
        throw new HubError(403, "FORBIDDEN_TARGET_SCOPE", "Target listener is private to another user");
      }
    }

    const msgId = opts?.msgID ?? null;
    const signalId = typeof parsedSignal?.signal_id === "string" ? parsedSignal.signal_id : null;
    let seq = 0;
    let duplicate = false;
    let createdMs = 0;
    const buf = Buffer.from(dataBytes);
    this.transaction(() => {
      if (msgId) {
        const existing = this.db.prepare("SELECT seq FROM hub_signals WHERE msg_id = ?").get(msgId) as { seq: number | bigint } | undefined;
        if (existing) {
          seq = Number(existing.seq);
          duplicate = true;
          return;
        }
      }
      createdMs = this.now();
      const res = this.db.prepare(
        "INSERT INTO hub_signals (subject, msg_id, signal_id, created_ms, data, acked) VALUES (?, ?, ?, ?, ?, 0)",
      ).run(subject, msgId, signalId, createdMs, buf);
      seq = Number(res.lastInsertRowid);
      this.recordAudit(
        caller?.principal_id ?? "anonymous",
        "send",
        targetId,
        {
          listener_id: targetId,
          ...(signalId ? { signal_id: signalId } : {}),
          seq,
        },
      );
    });

    if (!duplicate) {
      const ev: HubSignalEvent = {
        seq,
        subject,
        created_ms: createdMs,
        data: buf,
        acked_at_ms: null,
      };
      this.events.emit("signal", ev);
      if (parsedSignal) {
        void this.deliverSignalToHumanChannels(seq, targetId, createdMs, parsedSignal);
      }
    }
    return { seq, duplicate, stream: SIGNALS_STREAM };
  }

  ncPublish(subject: string, dataBytes: Uint8Array, caller: Principal | null): { ok: true } {
    if (!isAllowedCitoSubject(subject)) {
      throw new HubError(403, "PERMISSIONS_VIOLATION", `PERMISSIONS_VIOLATION: subject '${subject}' is not permitted`);
    }
    if (subject.startsWith("cito.signal.")) {
      this.jsPublish(subject, dataBytes, undefined, caller);
      return { ok: true };
    }
    const signalId = subject.slice("cito.ack.".length);
    const buf = Buffer.from(dataBytes);
    this.recordAudit(caller?.principal_id ?? "anonymous", "ack", signalId, { signal_id: signalId });
    const ev: HubNcEvent = { subject, data: buf };
    this.events.emit("nc", ev);
    return { ok: true };
  }

  ncSubscribeCheck(subject: string, _caller: Principal | null): { ok: true } {
    if (!isAllowedCitoSubject(subject)) {
      throw new HubError(403, "PERMISSIONS_VIOLATION", `PERMISSIONS_VIOLATION: subject '${subject}' is not permitted`);
    }
    return { ok: true };
  }

  private mapBacklogRow(r: {
    seq: number | bigint;
    subject: string;
    created_ms: number | bigint;
    data: Uint8Array;
    acked_at_ms?: number | bigint | null;
  }): { seq: number; subject: string; created_ms: number; data_b64: string; acked_at_ms: number | null } {
    const ackedAtMs = r.acked_at_ms != null ? Number(r.acked_at_ms) : null;
    let dataBuf = Buffer.from(r.data);
    if (ackedAtMs !== null) {
      try {
        const parsed: unknown = JSON.parse(dataBuf.toString("utf8"));
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
          dataBuf = Buffer.from(JSON.stringify({ ...(parsed as Record<string, unknown>), acked_at_ms: ackedAtMs }), "utf8");
        }
      } catch {}
    }
    return {
      seq: Number(r.seq),
      subject: r.subject,
      created_ms: Number(r.created_ms),
      data_b64: dataBuf.toString("base64"),
      acked_at_ms: ackedAtMs,
    };
  }

  consumerAdd(
    stream: string,
    config: {
      durable_name?: string;
      name?: string;
      filter_subject?: string;
      deliver_policy?: string;
      opt_start_seq?: number;
      [key: string]: unknown;
    } = {},
    caller: Principal | null,
  ): {
    name: string;
    stream_name: string;
    config: Record<string, unknown>;
    backlog: { seq: number; subject: string; created_ms: number; data_b64: string; acked_at_ms: number | null }[];
  } {
    const filterSubject = config.filter_subject;
    if (this.authMode === "oidc" && filterSubject?.startsWith("cito.signal.")) {
      const targetId = filterSubject.slice("cito.signal.".length);
      if (!this.canConsumeSignal(targetId, caller)) {
        throw new HubError(403, "PERMISSIONS_VIOLATION", "PERMISSIONS_VIOLATION: cannot consume signals for another principal");
      }
    }
    const name = config.durable_name ?? config.name ?? ("c_" + randomUUID().replaceAll("-", "").slice(0, 12));
    const startSeq = config.deliver_policy === "by_start_sequence" && typeof config.opt_start_seq === "number"
      ? Math.max(1, config.opt_start_seq)
      : 1;
    const rows = (
      filterSubject && !filterSubject.includes("*") && !filterSubject.includes(">")
        ? this.db.prepare("SELECT seq, subject, created_ms, data, acked_at_ms FROM hub_signals WHERE subject = ? AND seq >= ? ORDER BY seq ASC").all(filterSubject, startSeq)
        : this.db.prepare("SELECT seq, subject, created_ms, data, acked_at_ms FROM hub_signals WHERE seq >= ? ORDER BY seq ASC").all(startSeq)
    ) as {
      seq: number | bigint;
      subject: string;
      created_ms: number | bigint;
      data: Uint8Array;
      acked_at_ms: number | bigint | null;
    }[];
    const filtered = rows.filter(r => subjectMatchesFilter(r.subject, filterSubject) && (
      !r.subject.startsWith("cito.signal.") || this.canConsumeSignal(r.subject.slice("cito.signal.".length), caller)
    ));
    this.consumers.set(name, {
      name,
      stream,
      filterSubject,
      startSeq,
      cursorSeq: startSeq,
      ackedSeqs: new Set<number>(),
      principalId: caller?.principal_id ?? null,
    });
    return {
      name,
      stream_name: stream,
      config,
      backlog: filtered.map(r => this.mapBacklogRow(r)),
    };
  }

  consumerGetBacklog(
    name: string,
    caller: Principal | null,
  ): {
    filterSubject?: string;
    startSeq: number;
    backlog: { seq: number; subject: string; created_ms: number; data_b64: string; acked_at_ms: number | null }[];
  } {
    const consumer = this.consumers.get(name);
    const filterSubject = consumer?.filterSubject;
    const startSeq = consumer?.startSeq ?? 1;
    const rows = (
      filterSubject && !filterSubject.includes("*") && !filterSubject.includes(">")
        ? this.db.prepare("SELECT seq, subject, created_ms, data, acked_at_ms FROM hub_signals WHERE subject = ? AND seq >= ? ORDER BY seq ASC").all(filterSubject, startSeq)
        : this.db.prepare("SELECT seq, subject, created_ms, data, acked_at_ms FROM hub_signals WHERE seq >= ? ORDER BY seq ASC").all(startSeq)
    ) as {
      seq: number | bigint;
      subject: string;
      created_ms: number | bigint;
      data: Uint8Array;
      acked_at_ms: number | bigint | null;
    }[];
    const filtered = rows.filter(r => {
      const seqNum = Number(r.seq);
      if (consumer?.ackedSeqs.has(seqNum)) return false;
      if (!subjectMatchesFilter(r.subject, filterSubject)) return false;
      if (r.subject.startsWith("cito.signal.") && !this.canConsumeSignal(r.subject.slice("cito.signal.".length), caller)) return false;
      return true;
    });
    return {
      filterSubject,
      startSeq,
      backlog: filtered.map(r => this.mapBacklogRow(r)),
    };
  }

  consumerNext(
    _stream: string,
    name: string,
    caller: Principal | null,
  ): { seq: number; subject: string; created_ms: number; data_b64: string; acked_at_ms: number | null } | null {
    const { backlog } = this.consumerGetBacklog(name, caller);
    const consumer = this.consumers.get(name);
    for (const item of backlog) {
      if (!consumer || item.seq >= consumer.cursorSeq) {
        if (consumer) consumer.cursorSeq = item.seq + 1;
        return item;
      }
    }
    return null;
  }

  /** Transport cursor ack only — never sets business-level acked_at_ms. */
  consumerAck(_stream: string, name: string, seq: number, _caller: Principal | null): { acked: true } {
    const consumer = this.consumers.get(name);
    consumer?.ackedSeqs.add(seq);
    return { acked: true };
  }

  consumerDelete(_stream: string, name: string, _caller: Principal | null): { deleted: true } {
    this.consumers.delete(name);
    return { deleted: true };
  }

  signalClaimDispatch(
    params: { node_id: string; signal_ids: string[]; channel_id: string; lease_ms?: number },
    caller: Principal | null,
  ): { claimed: boolean; claimed_ids: string[] } {
    const nodeId = String(params.node_id ?? "");
    const channelId = String(params.channel_id ?? "");
    const signalIds = Array.isArray(params.signal_ids) ? params.signal_ids.map(String) : [];
    if (!nodeId || !channelId || signalIds.length === 0) {
      return { claimed: false, claimed_ids: [] };
    }
    if (this.authMode === "oidc" && !this.canConsumeSignal(nodeId, caller)) {
      throw new HubError(403, "PERMISSIONS_VIOLATION", "PERMISSIONS_VIOLATION: cannot claim signals for another principal");
    }
    const now = this.now();
    const leaseMs = Math.max(1000, Math.min(300_000, Number(params.lease_ms ?? 30_000)));
    const untilMs = now + leaseMs;
    const subject = "cito.signal." + nodeId;
    const claimedIds: string[] = [];

    this.transaction(() => {
      for (const sigId of signalIds) {
        let row = this.db
          .prepare("SELECT seq, claimed_by_channel, claim_until_ms, acked_at_ms FROM hub_signals WHERE subject = ? AND signal_id = ?")
          .get(subject, sigId) as {
            seq: number | bigint;
            claimed_by_channel: string | null;
            claim_until_ms: number | bigint | null;
            acked_at_ms: number | bigint | null;
          } | undefined;
        if (!row) {
          const candidates = this.db
            .prepare("SELECT seq, data, claimed_by_channel, claim_until_ms, acked_at_ms FROM hub_signals WHERE subject = ?")
            .all(subject) as {
              seq: number | bigint;
              data: Uint8Array;
              claimed_by_channel: string | null;
              claim_until_ms: number | bigint | null;
              acked_at_ms: number | bigint | null;
            }[];
          row = candidates.find(c => {
            try {
              const parsed = JSON.parse(Buffer.from(c.data).toString("utf8")) as { signal_id?: unknown };
              return parsed.signal_id === sigId;
            } catch {
              return false;
            }
          });
        }
        if (!row) {
          claimedIds.push(sigId);
          continue;
        }
        if (row.acked_at_ms !== null) {
          continue;
        }
        if (
          row.claimed_by_channel !== null &&
          row.claimed_by_channel !== channelId &&
          Number(row.claim_until_ms ?? 0) > now
        ) {
          continue;
        }
        this.db
          .prepare("UPDATE hub_signals SET claimed_by_channel = ?, claim_until_ms = ?, signal_id = COALESCE(signal_id, ?) WHERE seq = ?")
          .run(channelId, untilMs, sigId, Number(row.seq));
        claimedIds.push(sigId);
      }
    });

    return {
      claimed: claimedIds.length === signalIds.length && claimedIds.length > 0,
      claimed_ids: claimedIds,
    };
  }

  signalAck(
    params: { node_id: string; signal_ids: string[]; channel_id?: string },
    caller: Principal | null,
  ): { acked_count: number; acked_at_ms: number; signal_ids: string[] } {
    const nodeId = String(params.node_id ?? "");
    const signalIds = Array.isArray(params.signal_ids) ? params.signal_ids.map(String).filter(Boolean) : [];
    const channelId = typeof params.channel_id === "string" ? params.channel_id : undefined;
    if (!nodeId || signalIds.length === 0) {
      return { acked_count: 0, acked_at_ms: this.now(), signal_ids: [] };
    }
    if (this.authMode === "oidc" && !this.canConsumeSignal(nodeId, caller)) {
      throw new HubError(403, "PERMISSIONS_VIOLATION", "PERMISSIONS_VIOLATION: cannot acknowledge signals for another principal");
    }
    const ackedAtMs = this.now();
    const subject = "cito.signal." + nodeId;
    let ackedCount = 0;

    this.transaction(() => {
      for (const sigId of signalIds) {
        const res = this.db
          .prepare(`
            UPDATE hub_signals
            SET acked = 1, acked_at_ms = COALESCE(acked_at_ms, ?)
            WHERE subject = ? AND signal_id = ?
          `)
          .run(ackedAtMs, subject, sigId);
        if (Number(res.changes) > 0) {
          ackedCount += Number(res.changes);
          continue;
        }
        const candidates = this.db
          .prepare("SELECT seq, data FROM hub_signals WHERE subject = ? AND acked_at_ms IS NULL")
          .all(subject) as { seq: number | bigint; data: Uint8Array }[];
        for (const c of candidates) {
          try {
            const parsed = JSON.parse(Buffer.from(c.data).toString("utf8")) as { signal_id?: unknown };
            if (parsed.signal_id === sigId) {
              this.db
                .prepare("UPDATE hub_signals SET acked = 1, acked_at_ms = COALESCE(acked_at_ms, ?), signal_id = ? WHERE seq = ?")
                .run(ackedAtMs, sigId, Number(c.seq));
              ackedCount += 1;
            }
          } catch {}
        }
      }
      this.recordAudit(caller?.principal_id ?? "anonymous", "ack", nodeId, {
        listener_id: nodeId,
        signal_ids: signalIds,
        ...(channelId ? { channel_id: channelId } : {}),
      });
    });

    const ackEventPayload = {
      type: "node_ack",
      node_id: nodeId,
      signal_ids: signalIds,
      acked_at_ms: ackedAtMs,
      channel_id: channelId ?? null,
    };
    const ev: HubNcEvent = {
      subject: "cito.ack." + nodeId,
      data: Buffer.from(JSON.stringify(ackEventPayload), "utf8"),
    };
    this.events.emit("nc", ev);
    void this.notifyHumanChannelsAcked(nodeId, signalIds, ackedAtMs, channelId ?? null);
    return { acked_count: ackedCount, acked_at_ms: ackedAtMs, signal_ids: signalIds };
  }

  private async notifyHumanChannelsAcked(
    nodeId: string,
    signalIds: string[],
    ackedAtMs: number,
    channelId: string | null,
  ): Promise<void> {
    if (this.closed) return;
    const kvRow = this.db.prepare("SELECT principal_id FROM hub_kv WHERE key = ?").get(nodeId) as {
      principal_id?: string | null;
    } | undefined;
    const principalId = kvRow?.principal_id;
    if (!principalId) return;
    const bindings = this.db
      .prepare("SELECT plugin_id, user_config_json FROM hub_human_channels WHERE principal_id = ? AND enabled = 1")
      .all(principalId) as { plugin_id: string; user_config_json: string }[];
    for (const b of bindings) {
      const plugin = getHumanChannelPlugin(b.plugin_id);
      if (!plugin?.onAcked) continue;
      const extMap: Record<string, string> = {};
      for (const sigId of signalIds) {
        const dRow = this.db
          .prepare("SELECT external_msg_id FROM hub_channel_deliveries WHERE plugin_id = ? AND principal_id = ? AND signal_id = ?")
          .get(plugin.id, principalId, sigId) as { external_msg_id?: string | null } | undefined;
        if (dRow?.external_msg_id) {
          extMap[sigId] = dRow.external_msg_id;
        }
      }
      const { config: hubConfig } = this.getPluginServiceRow(plugin.id);
      let userConfig: Record<string, unknown> = {};
      try { userConfig = JSON.parse(b.user_config_json) as Record<string, unknown>; } catch {}
      await Promise.resolve(
        plugin.onAcked({
          principal_id: principalId,
          signal_ids: signalIds,
          acked_at_ms: ackedAtMs,
          channel_id: channelId,
          external_msg_ids: extMap,
          hubConfig,
          userConfig,
          fetchFn: this.fetchFn,
        }),
      ).catch(() => {});
    }
  }

  humanChannelsList(caller: Principal | null): { channels: UiChannelSetupSummary[] } {
    const p = caller ?? localPrincipal();
    const channels: UiChannelSetupSummary[] = [];
    for (const plugin of getHumanChannelPlugins()) {
      if (plugin.id === "human") continue;
      const manifest = plugin.manifest;
      const { config: hubConfig } = this.getPluginServiceRow(plugin.id);
      const userRow = this.db
        .prepare("SELECT enabled, user_config_json, display_label FROM hub_human_channels WHERE principal_id = ? AND plugin_id = ?")
        .get(p.principal_id, plugin.id) as {
          enabled: number | bigint;
          user_config_json: string;
          display_label: string | null;
        } | undefined;
      let userConfig: Record<string, unknown> = {};
      if (userRow?.user_config_json) {
        try { userConfig = JSON.parse(userRow.user_config_json) as Record<string, unknown>; } catch {}
      }

      const rawFields = manifest?.fields ?? [];
      const hubFields = rawFields.filter(f => f.scope === "hub");
      const userFields = rawFields.filter(f => f.scope === "user");
      const allHubReady = hubFields.every(f => !f.required || (typeof hubConfig[f.key] === "string" && String(hubConfig[f.key]).trim().length > 0));
      const allUserReady = userFields.every(f => !f.required || (typeof userConfig[f.key] === "string" && String(userConfig[f.key]).trim().length > 0));
      const hubProvided = this.authMode === "oidc" && allHubReady;

      const fields: UiChannelFieldSummary[] = rawFields.map(f => {
        const val = f.scope === "hub" ? String(hubConfig[f.key] ?? "") : String(userConfig[f.key] ?? "");
        const configured = val.trim().length > 0;
        return {
          key: f.key,
          scope: f.scope,
          label: f.label,
          type: f.type,
          required: Boolean(f.required),
          placeholder: f.placeholder ?? "",
          value: f.scope === "hub" && hubProvided ? "" : val,
          configured,
          ...(f.scope === "hub" && hubProvided ? { hidden: true } : {}),
        };
      });

      const activePairRow = this.db
        .prepare("SELECT code FROM hub_pairing_codes WHERE principal_id = ? AND plugin_id = ? AND consumed_ms IS NULL AND expires_ms > ? ORDER BY created_ms DESC LIMIT 1")
        .get(p.principal_id, plugin.id, this.now()) as { code?: string } | undefined;

      const enabled = userRow ? Number(userRow.enabled) === 1 : true;
      const configured = allHubReady && allUserReady;
      const status: UiChannelSetupSummary["status"] = configured
        ? "ready"
        : allHubReady && hubFields.length > 0
        ? "hub_ready"
        : "not_configured";
      const detail = configured
        ? (userRow?.display_label ? `Connected (${userRow.display_label})` : "Connected and ready for Cito notifications")
        : allHubReady && hubFields.length > 0
        ? "Hub Service: ● Ready · Pair or enter your account below"
        : "Configure credentials below to receive notifications";

      channels.push({
        id: plugin.id,
        display_name: manifest?.displayName ?? plugin.id,
        sf_symbol: manifest?.sfSymbol ?? "paperplane.fill",
        tint: manifest?.tint ?? "blue",
        enabled,
        configured,
        status,
        detail,
        display_label: userRow?.display_label ?? null,
        hub_provided: hubProvided,
        pairing_supported: Boolean(manifest?.pairing?.supported),
        pairing_action_label: manifest?.pairing?.actionLabel ?? "Pair Account…",
        pairing_code: activePairRow?.code ?? null,
        pairing_instruction: activePairRow?.code ? `Send "/start ${activePairRow.code}" to @your_cito_bot` : null,
        fields,
      });
    }
    return { channels };
  }

  humanChannelsConfigure(
    params: {
      plugin_id: string;
      enabled?: boolean;
      hub_config?: Record<string, unknown>;
      user_config?: Record<string, unknown>;
      display_label?: string | null;
    },
    caller: Principal | null,
  ): { channels: UiChannelSetupSummary[] } {
    const p = caller ?? localPrincipal();
    const pluginId = String(params.plugin_id ?? "");
    const plugin = getHumanChannelPlugin(pluginId);
    if (!plugin) {
      throw new HubError(400, "INVALID_INPUT", `Unknown human channel plugin: ${pluginId}`);
    }
    this.transaction(() => {
      if (params.hub_config && typeof params.hub_config === "object" && !Array.isArray(params.hub_config)) {
        const existing = this.getPluginServiceRow(plugin.id).config;
        const nextHub = { ...existing };
        for (const [k, v] of Object.entries(params.hub_config)) {
          if (v !== undefined) nextHub[k] = v;
        }
        this.savePluginServiceConfig(plugin.id, nextHub);
      }
      if (params.user_config || params.enabled !== undefined || params.display_label !== undefined) {
        this.upsertHumanChannelBinding({
          principalId: p.principal_id,
          ownerUser: p.owner_user,
          pluginId: plugin.id,
          enabled: params.enabled ?? true,
          userConfig: (params.user_config && typeof params.user_config === "object" && !Array.isArray(params.user_config))
            ? params.user_config
            : {},
          ...(params.display_label !== undefined ? { displayLabel: params.display_label } : {}),
        });
      }
    });
    const list = this.humanChannelsList(p);
    const summary = list.channels.find(c => c.id === plugin.id);
    if (summary) {
      this.ensureHumanChannelNodeAttachment(p, plugin, summary.configured && summary.enabled);
    }
    this.syncPluginBackgroundServices();
    return list;
  }

  async humanChannelsTest(
    params: {
      plugin_id: string;
      hub_config?: Record<string, unknown>;
      user_config?: Record<string, unknown>;
    },
    caller: Principal | null,
  ): Promise<{ ok: boolean; detail: string; botUsername?: string }> {
    const p = caller ?? localPrincipal();
    const pluginId = String(params.plugin_id ?? "");
    const plugin = getHumanChannelPlugin(pluginId);
    if (!plugin) {
      throw new HubError(400, "INVALID_INPUT", `Unknown human channel plugin: ${pluginId}`);
    }
    if (!plugin.testConnection) {
      return { ok: true, detail: "Channel ready" };
    }
    const { config: storedHub } = this.getPluginServiceRow(plugin.id);
    const userRow = this.db
      .prepare("SELECT user_config_json FROM hub_human_channels WHERE principal_id = ? AND plugin_id = ?")
      .get(p.principal_id, plugin.id) as { user_config_json?: string } | undefined;
    let storedUser: Record<string, unknown> = {};
    if (userRow?.user_config_json) {
      try { storedUser = JSON.parse(userRow.user_config_json) as Record<string, unknown>; } catch {}
    }
    const hubConfig = { ...storedHub, ...(params.hub_config ?? {}) };
    const userConfig = { ...storedUser, ...(params.user_config ?? {}) };
    return await plugin.testConnection({
      hubConfig,
      userConfig,
      owner_user: p.owner_user,
      fetchFn: this.fetchFn,
    });
  }

  async humanChannelPairStart(
    params: { plugin_id: string; hub_config?: Record<string, unknown> },
    caller: Principal | null,
  ): Promise<{ code: string; expires_at_ms: number; instruction: string }> {
    const p = caller ?? localPrincipal();
    const pluginId = String(params.plugin_id ?? "");
    const plugin = getHumanChannelPlugin(pluginId);
    if (!plugin || !plugin.manifest?.pairing?.supported) {
      throw new HubError(400, "INVALID_INPUT", `Pairing is not supported for plugin '${pluginId}'`);
    }
    if (params.hub_config && typeof params.hub_config === "object" && !Array.isArray(params.hub_config)) {
      const existing = this.getPluginServiceRow(plugin.id).config;
      const merged = { ...existing, ...params.hub_config };
      this.savePluginServiceConfig(plugin.id, merged);
    }
    const now = this.now();
    const recentCount = Number(
      (
        this.db
          .prepare("SELECT COUNT(*) AS n FROM hub_pairing_codes WHERE principal_id = ? AND plugin_id = ? AND created_ms > ?")
          .get(p.principal_id, plugin.id, now - 60_000) as { n: number | bigint }
      ).n,
    );
    if (recentCount >= 5) {
      throw new HubError(429, "INVALID_INPUT", "Too many pairing code requests; please wait a minute");
    }
    const code = generatePairingCode();
    const expiresMs = now + 5 * 60_000;
    this.db
      .prepare(
        "INSERT INTO hub_pairing_codes (code, plugin_id, principal_id, owner_user, created_ms, expires_ms, consumed_ms) VALUES (?, ?, ?, ?, ?, ?, NULL)",
      )
      .run(code, plugin.id, p.principal_id, p.owner_user, now, expiresMs);
    const { config: hubConfig } = this.getPluginServiceRow(plugin.id);
    const instruction = plugin.formatPairingInstruction
      ? await plugin.formatPairingInstruction({ code, hubConfig, fetchFn: this.fetchFn })
      : `Send "/start ${code}"`;
    this.syncPluginBackgroundServices();
    return { code, expires_at_ms: expiresMs, instruction };
  }

  handleRpc(op: string, params: Record<string, unknown>, caller: Principal | null): unknown {
    switch (op) {
      case "stream_info":
        return this.streamInfo(String(params.stream ?? ""));
      case "stream_add":
        return this.streamAdd((params.config ?? {}) as Partial<StreamConfig> & { name: string });
      case "stream_update":
        return this.streamUpdate(String(params.stream ?? ""), (params.patch ?? {}) as Partial<StreamConfig>);
      case "stream_get_message":
        return this.streamGetMessage(String(params.stream ?? ""), Number(params.seq ?? 0), caller);
      case "kv_get":
        return this.kvGet(String(params.key ?? ""), caller);
      case "kv_create":
        return this.kvCreate(String(params.key ?? ""), Buffer.from(String(params.data_b64 ?? ""), "base64"), caller);
      case "kv_update":
        return this.kvUpdate(
          String(params.key ?? ""),
          Buffer.from(String(params.data_b64 ?? ""), "base64"),
          Number(params.revision ?? 0),
          caller,
        );
      case "kv_put":
        return this.kvPut(String(params.key ?? ""), Buffer.from(String(params.data_b64 ?? ""), "base64"), caller);
      case "kv_delete":
        return this.kvDelete(String(params.key ?? ""), caller);
      case "kv_keys":
        return this.kvKeys(caller);
      case "kv_snapshot":
        return this.kvSnapshot(typeof params.key === "string" ? params.key : undefined, caller);
      case "js_publish":
        return this.jsPublish(
          String(params.subject ?? ""),
          Buffer.from(String(params.data_b64 ?? ""), "base64"),
          params.opts as { msgID?: string } | undefined,
          caller,
        );
      case "nc_publish":
        return this.ncPublish(String(params.subject ?? ""), Buffer.from(String(params.data_b64 ?? ""), "base64"), caller);
      case "nc_subscribe_check":
        return this.ncSubscribeCheck(String(params.subject ?? ""), caller);
      case "consumer_add":
        return this.consumerAdd(String(params.stream ?? SIGNALS_STREAM), (params.config ?? {}) as Record<string, unknown>, caller);
      case "consumer_backlog":
        return this.consumerGetBacklog(String(params.name ?? ""), caller);
      case "consumer_next":
        return this.consumerNext(String(params.stream ?? SIGNALS_STREAM), String(params.name ?? ""), caller);
      case "consumer_ack":
        return this.consumerAck(String(params.stream ?? SIGNALS_STREAM), String(params.name ?? ""), Number(params.seq ?? 0), caller);
      case "consumer_delete":
        return this.consumerDelete(String(params.stream ?? SIGNALS_STREAM), String(params.name ?? ""), caller);
      case "signal_claim_dispatch":
        return this.signalClaimDispatch(
          params as { node_id: string; signal_ids: string[]; channel_id: string; lease_ms?: number },
          caller,
        );
      case "signal_ack":
        return this.signalAck(
          params as { node_id: string; signal_ids: string[]; channel_id?: string },
          caller,
        );
      case "human_channels_list":
        return this.humanChannelsList(caller);
      case "human_channels_configure":
        return this.humanChannelsConfigure(
          params as {
            plugin_id: string;
            enabled?: boolean;
            hub_config?: Record<string, unknown>;
            user_config?: Record<string, unknown>;
            display_label?: string | null;
          },
          caller,
        );
      case "human_channels_test":
        return this.humanChannelsTest(
          params as {
            plugin_id: string;
            hub_config?: Record<string, unknown>;
            user_config?: Record<string, unknown>;
          },
          caller,
        );
      case "human_channel_pair_start":
        return this.humanChannelPairStart(
          params as { plugin_id: string; hub_config?: Record<string, unknown> },
          caller,
        );
      case "human_channel_poll":
        return this.pollHumanChannel(String(params.plugin_id ?? ""));
      case "account_info": {
        const kvCount = Number((this.db.prepare("SELECT COUNT(*) AS n FROM hub_kv").get() as { n: number | bigint }).n);
        const sigCount = Number((this.db.prepare("SELECT COUNT(*) AS n FROM hub_signals").get() as { n: number | bigint }).n);
        return {
          streams: kvCount === 0 && sigCount === 0 ? 0 : 2,
          consumers: this.consumers.size,
          memory: 0,
          storage: 0,
        };
      }
      default:
        throw new HubError(400, "INVALID_INPUT", `Unknown hub RPC op: ${op}`);
    }
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    for (const srv of this.runningPluginServices.values()) {
      try { void srv.stop(); } catch {}
    }
    this.runningPluginServices.clear();
    this.events.removeAllListeners();
    try { this.db.close(); } catch {}
  }
}
