import { escapeWakeCard, normalizeSlug } from "../shared/index.js";

/** Signal display fields are already sanitized at admission; escape only at rendering. */
export interface WakeSignal {
  signal_id: string;
  from_listener_id?: string | null;
  from_user: string;
  from_host: string;
  from_title: string;
  created_at: string;
  message: string;
}
export interface WakeTarget { listener_id: string; title: string }

function formatSender(signal: WakeSignal): string {
  const host = signal.from_host.replace(/\.local$/i, "") || signal.from_host;
  const title = signal.from_title.trim();
  const slug = signal.from_listener_id?.match(/^([a-z0-9]+(?:-[a-z0-9]+)*)-[a-z2-7]{12}$/)?.[1];
  const titlePart = title && !(signal.from_listener_id && normalizeSlug(title) === slug)
    ? " · " + escapeWakeCard(title) : "";
  const replyPart = signal.from_listener_id ? " [↩ " + escapeWakeCard(signal.from_listener_id) + "]" : "";
  return `${escapeWakeCard(signal.from_user)}@${escapeWakeCard(host)}${titlePart}${replyPart}`;
}
export function formatWakeCard(target: WakeTarget, signals: readonly WakeSignal[]): string {
  const ackIds = escapeWakeCard(JSON.stringify(signals.map(signal => signal.signal_id)));
  const footer = `↳ Ack: cito_inbox(ack_ids: ${ackIds})`;
  if (signals.length === 1) {
    const signal = signals[0]!;
    return `🔔 Cito · ${formatSender(signal)}\n\n${escapeWakeCard(signal.message)}\n\n${footer}`;
  }
  const targetTitle = target.title.trim() ? " · " + escapeWakeCard(target.title.trim()) : "";
  const body = signals.map((signal, index) =>
    `[${index + 1}] ${formatSender(signal)}\n${escapeWakeCard(signal.message)}`).join("\n\n");
  return `🔔 Cito · ${signals.length} 条唤醒消息${targetTitle}\n\n${body}\n\n${footer}`;
}
