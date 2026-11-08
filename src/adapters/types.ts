import type { JSONRPCMessage } from "@modelcontextprotocol/sdk/types.js";
import type { CitoConfig } from "../shared/index.js";
import type { ListenerRow } from "../shared/state-types.js";
import type { RepoMetadata } from "./git.js";

export type AdapterTier = "stable" | "experimental" | "custom";
export type AdapterTint = "indigo" | "teal" | "orange" | "blue" | "purple" | "green" | "pink" | "gray";

export interface PluginFieldDescriptor {
  key: string;
  scope: "hub" | "user";
  label: string;
  type: "string" | "secret" | "url" | "boolean";
  required?: boolean;
  placeholder?: string;
}

export type PluginProtocol =
  | "socket-bridge"
  | "sqlite-cli"
  | "transcript-hook"
  | "desktop-ui"
  | "http-bot-poll"
  | "external-driver";

export interface PluginManifest {
  id: string;
  kind?: "agent" | "human";
  displayName: string;
  tier?: AdapterTier;
  sfSymbol?: string;
  tint?: AdapterTint;
  cliFlag?: boolean;
  protocol?: PluginProtocol;
  driver?: string;
  native?: Record<string, unknown>;
  fields?: readonly PluginFieldDescriptor[];
  pairing?: { supported: boolean; actionLabel?: string };
}

export interface UiHarnessMetadata {
  id: string;
  display_name: string;
  sf_symbol: string;
  tint: AdapterTint;
  tier: AdapterTier;
  order: number;
}

export interface SetupFileAction {
  target: string;
  path: string;
  status: "created" | "updated" | "unchanged" | "skipped" | "conflict";
  backupPath?: string;
  detail: string;
}

export interface HarnessSetupInspection {
  id: string;
  displayName: string;
  tier: AdapterTier;
  installed: boolean;
  configured: boolean;
  paths: string[];
}

export interface HarnessSetupDescriptor {
  inspect(env: NodeJS.ProcessEnv): Promise<{ installed: boolean; configured: boolean; paths: string[] }>;
  plan(env: NodeJS.ProcessEnv, options: { force?: boolean }): Promise<SetupFileAction[]>;
  apply(env: NodeJS.ProcessEnv, options: { force?: boolean }): Promise<SetupFileAction[]>;
  remove(env: NodeJS.ProcessEnv): Promise<SetupFileAction[]>;
}

export interface HarnessCliDescriptor {
  consumesNextArg?(cmd: "listen" | "unlisten", nextArg: string, config: CitoConfig): boolean;
  resolveMeta(
    cmd: "listen" | "unlisten",
    explicitId: string | undefined,
    cwd: string,
    config: CitoConfig,
  ): Promise<Record<string, unknown>> | Record<string, unknown>;
}

export interface StdioChannelBridge {
  readonly socketPath: string;
  sessionId(): Promise<string>;
  forward(message: JSONRPCMessage): JSONRPCMessage;
  close(): Promise<void>;
}

export interface Caller extends RepoMetadata {
  harness: string;
  session_id: string;
  cwd: string;
  endpoint_pid?: number;
}
export interface AvailableSessionSummary {
  harness: string;
  session_id: string;
  title: string;
  cwd: string;
  branch?: string | null;
  endpoint_pid?: number;
  updated_at_ms?: number;
  listening_hubs?: string[];
}
export interface CachedSessionQuery {
  harness?: string;
  sessionId?: string;
  endpointPid?: number;
}
export interface CallerDetectionContext {
  meta: Record<string, unknown>;
  headers?: Record<string, string | string[] | undefined>;
  httpClientPid?: number;
  httpTransport?: boolean;
  requireOpen?: boolean;
  lookupCachedSession?: (query: CachedSessionQuery) => Caller | undefined | Promise<Caller | undefined>;
}
export type CallerInput = CallerDetectionContext | Record<string, unknown>;

export type ProbeOutcome =
  | { status: "alive" | "switched" | "closed_definitive" }
  | { status: "transient_error"; error: unknown };
export type DeliverOutcome = "delivered" | "delivered_unconfirmed" | "driver_unavailable" | "session_switched";
export interface DeliverContext {
  batchId: string;
  card: string;
  queueWritten: boolean;
  markQueueWritten: () => void;
  runEffect: (
    submit: () => Promise<void>,
    opts: {
      attempts: number;
      pause: number[];
      onSigtermTimeout?: () => void;
    },
  ) => Promise<boolean>;
  config: CitoConfig;
  signal: AbortSignal;
}
export interface HarnessAdapter {
  readonly id: string;
  readonly manifest?: PluginManifest;
  readonly pidKeys?: readonly string[];
  readonly cli?: HarnessCliDescriptor;
  readonly setup?: HarnessSetupDescriptor;
  startStdioChannel?(
    config: CitoConfig,
    transport: { send(message: JSONRPCMessage): Promise<void> },
    options?: { endpointPid?: number; cwd?: string; env?: NodeJS.ProcessEnv },
  ): Promise<StdioChannelBridge | undefined>;
  detectExplicitCaller?(ctx: CallerDetectionContext, config: CitoConfig): Promise<Caller | null>;
  discoverAvailableSessions?(config: CitoConfig, signal?: AbortSignal): Promise<AvailableSessionSummary[]>;
  probe(row: ListenerRow, config: CitoConfig, signal: AbortSignal): Promise<ProbeOutcome>;
  deliver(row: ListenerRow, ctx: DeliverContext): Promise<DeliverOutcome>;
  updateBadge?(row: ListenerRow, badge: string | undefined, config: CitoConfig, signal?: AbortSignal): Promise<void>;
}

export interface HumanNotificationPayload {
  signal_id: string;
  seq: number;
  principal_id: string;
  owner_user: string;
  from_user: string;
  from_host_name: string;
  from_harness: string | null;
  from_listener_id: string | null;
  from_title: string;
  from_about: string | null;
  message: string;
  in_reply_to: string | null;
  created_at: string;
}

export interface HumanChannelDeliveryResult {
  delivered: boolean;
  externalMessageId?: string | null;
  detail?: string;
}

export interface HumanChannelServiceContext {
  getHubConfig(): Record<string, unknown>;
  getServiceState(): Record<string, unknown>;
  setServiceState(state: Record<string, unknown>): void | Promise<void>;
  completePairing(
    code: string,
    result: { userConfig: Record<string, unknown>; displayLabel?: string },
  ): { ok: boolean; principal_id?: string; owner_user?: string; error?: string } | Promise<{ ok: boolean; principal_id?: string; owner_user?: string; error?: string }>;
  resolvePrincipalByUserConfig(
    matchKey: string,
    matchValue: string,
  ): { principal_id: string; owner_user: string; userConfig: Record<string, unknown> } | undefined;
  resolveReplyTarget(
    principalId: string,
    options: { replyToExternalMsgId?: string | null; maxAgeMs?: number },
  ): { signal_id: string; from_listener_id: string; from_title?: string; from_user?: string } | undefined;
  sendReplyFromHuman(
    principalId: string,
    options: { to_listener_id: string; message: string; in_reply_to?: string | null },
  ): Promise<{ ok: boolean; signal_id?: string; to_label?: string; error?: string }>;
  fetchFn?: typeof fetch;
}

export interface HumanChannelPlugin {
  readonly id: string;
  readonly kind?: "human";
  readonly manifest?: PluginManifest;
  formatPairingInstruction?(options: {
    code: string;
    hubConfig: Record<string, unknown>;
    botUsername?: string;
    fetchFn?: typeof fetch;
  }): Promise<string> | string;
  testConnection?(options: {
    hubConfig: Record<string, unknown>;
    userConfig: Record<string, unknown>;
    owner_user: string;
    fetchFn?: typeof fetch;
  }): Promise<{ ok: boolean; detail: string; botUsername?: string }>;
  sendNotification(
    notification: HumanNotificationPayload,
    options: {
      hubConfig: Record<string, unknown>;
      userConfig: Record<string, unknown>;
      fetchFn?: typeof fetch;
    },
  ): Promise<HumanChannelDeliveryResult>;
  onAcked?(event: {
    principal_id: string;
    signal_ids: string[];
    acked_at_ms: number;
    channel_id?: string | null;
    external_msg_ids?: Record<string, string>;
    hubConfig: Record<string, unknown>;
    userConfig: Record<string, unknown>;
    fetchFn?: typeof fetch;
  }): Promise<void> | void;
  pollOnce?(ctx: HumanChannelServiceContext): Promise<number>;
  startService?(ctx: HumanChannelServiceContext): Promise<{ stop(): Promise<void> | void } | void>;
}

export type PluginFactory =
  | HarnessAdapter
  | HumanChannelPlugin
  | ((manifest: PluginManifest) => HarnessAdapter | HumanChannelPlugin | Promise<HarnessAdapter | HumanChannelPlugin>);
