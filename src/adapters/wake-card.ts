import { escapeWakeCard } from "../shared/index.js";

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
const attribute = (value: string): string => escapeWakeCard(value).replace(/"/g, "&quot;");

export function formatWakeCard(target: WakeTarget, signals: readonly WakeSignal[]): string {
  const body = signals.map(signal =>
    `<signal id="${attribute(signal.signal_id)}" from="${attribute(signal.from_listener_id || "unbound")}" from_user="${attribute(signal.from_user)}" from_host="${attribute(signal.from_host)}" from_title="${attribute(signal.from_title)}" ts="${attribute(signal.created_at)}">\n${escapeWakeCard(signal.message)}\n</signal>`
  ).join("\n");
  // Escape the rendered JSON as well: IDs are interpolated data, not markup.
  const ackIds = escapeWakeCard(JSON.stringify(signals.map(signal => signal.signal_id)));
  return `<cito_signal to="${attribute(target.listener_id)}" target_title="${attribute(target.title)}" count="${signals.length}" wake="true">\n${body}\nPlease call cito_inbox(ack_ids: ${ackIds}) to acknowledge.</cito_signal>`;
}
