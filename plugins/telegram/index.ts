import { existsSync, readFileSync } from "node:fs";
import type {
  HumanChannelDeliveryResult,
  HumanChannelPlugin,
  HumanChannelServiceContext,
  HumanNotificationPayload,
  PluginManifest,
} from "../../src/adapters/types.js";

const localManifestUrl = new URL("./plugin.json", import.meta.url);
const rootManifestUrl = new URL("../../../plugins/telegram/plugin.json", import.meta.url);
export const telegramManifest: PluginManifest = JSON.parse(
  readFileSync(existsSync(localManifestUrl) ? localManifestUrl : rootManifestUrl, "utf8"),
) as PluginManifest;

function resolveApiBase(hubConfig: Record<string, unknown>): string {
  const raw = typeof hubConfig.apiBaseUrl === "string" && hubConfig.apiBaseUrl.trim()
    ? hubConfig.apiBaseUrl.trim().replace(/\/+$/, "")
    : "https://api.telegram.org";
  return raw;
}

async function callTelegramApi<T = unknown>(
  botToken: string,
  method: string,
  body: Record<string, unknown>,
  options: { apiBaseUrl?: string; fetchFn?: typeof fetch; signal?: AbortSignal } = {},
): Promise<{ ok: boolean; result?: T; description?: string; error_code?: number }> {
  const fetchImpl = options.fetchFn ?? globalThis.fetch;
  const base = (options.apiBaseUrl ?? "https://api.telegram.org").replace(/\/+$/, "");
  const url = `${base}/bot${botToken}/${method}`;
  const res = await fetchImpl(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
    signal: options.signal ?? AbortSignal.timeout(10_000),
  });
  const data = (await res.json()) as { ok?: boolean; result?: T; description?: string; error_code?: number };
  return {
    ok: data?.ok === true,
    result: data?.result,
    description: data?.description,
    error_code: data?.error_code,
  };
}

export class TelegramChannelPlugin implements HumanChannelPlugin {
  readonly id = "telegram";
  readonly kind = "human" as const;
  readonly manifest: PluginManifest = telegramManifest;

  async formatPairingInstruction(options: {
    code: string;
    hubConfig: Record<string, unknown>;
    botUsername?: string;
    fetchFn?: typeof fetch;
  }): Promise<string> {
    let botUsername = options.botUsername;
    const botToken = typeof options.hubConfig.botToken === "string" ? options.hubConfig.botToken.trim() : "";
    if (!botUsername && botToken) {
      try {
        const me = await callTelegramApi<{ username?: string }>(botToken, "getMe", {}, {
          apiBaseUrl: resolveApiBase(options.hubConfig),
          fetchFn: options.fetchFn,
        });
        if (me.ok && me.result?.username) {
          botUsername = me.result.username;
        }
      } catch {}
    }
    const handle = botUsername ? (botUsername.startsWith("@") ? botUsername : `@${botUsername}`) : "@your_cito_bot";
    return `Send "/start ${options.code}" to ${handle}`;
  }

  async testConnection(options: {
    hubConfig: Record<string, unknown>;
    userConfig: Record<string, unknown>;
    owner_user: string;
    fetchFn?: typeof fetch;
  }): Promise<{ ok: boolean; detail: string; botUsername?: string }> {
    const botToken = typeof options.hubConfig.botToken === "string" ? options.hubConfig.botToken.trim() : "";
    if (!botToken) {
      return { ok: false, detail: "Bot Token is required" };
    }
    const apiBaseUrl = resolveApiBase(options.hubConfig);
    try {
      const me = await callTelegramApi<{ username?: string }>(botToken, "getMe", {}, {
        apiBaseUrl,
        fetchFn: options.fetchFn,
      });
      if (!me.ok) {
        return { ok: false, detail: me.description || "Telegram getMe failed" };
      }
      const botUsername = me.result?.username;
      const chatId = typeof options.userConfig.chatId === "string" ? options.userConfig.chatId.trim() : "";
      if (chatId) {
        const sent = await callTelegramApi<{ message_id?: number }>(
          botToken,
          "sendMessage",
          {
            chat_id: chatId,
            text: `✅ Cito Telegram channel connected for @${options.owner_user}`,
          },
          { apiBaseUrl, fetchFn: options.fetchFn },
        );
        if (!sent.ok) {
          return { ok: false, detail: sent.description || "Telegram sendMessage failed", botUsername };
        }
      }
      return {
        ok: true,
        detail: chatId ? `Sent test message to chat ${chatId}` : `Bot @${botUsername ?? "bot"} verified`,
        ...(botUsername ? { botUsername } : {}),
      };
    } catch (error) {
      return { ok: false, detail: error instanceof Error ? error.message : String(error) };
    }
  }

  async sendNotification(
    notification: HumanNotificationPayload,
    options: {
      hubConfig: Record<string, unknown>;
      userConfig: Record<string, unknown>;
      fetchFn?: typeof fetch;
    },
  ): Promise<HumanChannelDeliveryResult> {
    const botToken = typeof options.hubConfig.botToken === "string" ? options.hubConfig.botToken.trim() : "";
    const chatId = typeof options.userConfig.chatId === "string" ? options.userConfig.chatId.trim() : "";
    if (!botToken || !chatId) {
      return { delivered: false, detail: "Missing botToken or chatId" };
    }
    const senderLabel = notification.from_title
      ? `@${notification.from_user}@${notification.from_host_name} (${notification.from_title})`
      : `@${notification.from_user}@${notification.from_host_name}`;
    const text = `🔔 Cito · ${senderLabel}\n\n${notification.message}`;
    try {
      const res = await callTelegramApi<{ message_id?: number }>(
        botToken,
        "sendMessage",
        { chat_id: chatId, text },
        { apiBaseUrl: resolveApiBase(options.hubConfig), fetchFn: options.fetchFn },
      );
      if (!res.ok) {
        return { delivered: false, detail: res.description || "Telegram sendMessage failed" };
      }
      return {
        delivered: true,
        externalMessageId: res.result?.message_id != null ? String(res.result.message_id) : null,
      };
    } catch (error) {
      return { delivered: false, detail: error instanceof Error ? error.message : String(error) };
    }
  }

  async onAcked(event: {
    principal_id: string;
    signal_ids: string[];
    acked_at_ms: number;
    channel_id?: string | null;
    external_msg_ids?: Record<string, string>;
    hubConfig: Record<string, unknown>;
    userConfig: Record<string, unknown>;
    fetchFn?: typeof fetch;
  }): Promise<void> {
    const botToken = typeof event.hubConfig.botToken === "string" ? event.hubConfig.botToken.trim() : "";
    const chatId = typeof event.userConfig.chatId === "string" ? event.userConfig.chatId.trim() : "";
    if (!botToken || !chatId || !event.external_msg_ids) return;
    const apiBaseUrl = resolveApiBase(event.hubConfig);
    for (const msgId of Object.values(event.external_msg_ids)) {
      if (!msgId) continue;
      await callTelegramApi(
        botToken,
        "editMessageReplyMarkup",
        { chat_id: chatId, message_id: Number(msgId), reply_markup: { inline_keyboard: [] } },
        { apiBaseUrl, fetchFn: event.fetchFn },
      ).catch(() => {});
    }
  }

  async pollOnce(ctx: HumanChannelServiceContext): Promise<number> {
    const hubConfig = ctx.getHubConfig();
    const botToken = typeof hubConfig.botToken === "string" ? hubConfig.botToken.trim() : "";
    if (!botToken) return 0;
    const apiBaseUrl = resolveApiBase(hubConfig);
    const state = ctx.getServiceState();
    const currentOffset = typeof state.update_offset === "number" && Number.isInteger(state.update_offset)
      ? state.update_offset
      : 0;

    const updatesRes = await callTelegramApi<
      {
        update_id: number;
        message?: {
          message_id: number;
          from?: { id?: number; username?: string };
          chat?: { id: number | string; type?: string };
          text?: string;
          reply_to_message?: { message_id: number };
        };
      }[]
    >(
      botToken,
      "getUpdates",
      {
        offset: currentOffset,
        timeout: 0,
        allowed_updates: ["message"],
      },
      { apiBaseUrl, fetchFn: ctx.fetchFn },
    );

    if (!updatesRes.ok || !Array.isArray(updatesRes.result) || updatesRes.result.length === 0) {
      return 0;
    }

    let nextOffset = currentOffset;
    let processed = 0;

    for (const upd of updatesRes.result) {
      if (typeof upd.update_id === "number" && upd.update_id + 1 > nextOffset) {
        nextOffset = upd.update_id + 1;
      }
      const msg = upd.message;
      if (!msg || !msg.chat || typeof msg.text !== "string") continue;
      const chatId = String(msg.chat.id);
      const text = msg.text.trim();
      if (!text) continue;

      const startMatch = /^\/start(?:@[A-Za-z0-9_]+)?\s+(cito-[a-z0-9-]+)$/i.exec(text);
      if (startMatch) {
        const code = startMatch[1]!.toLowerCase();
        const displayLabel = msg.from?.username ? `@${msg.from.username}` : `Chat ${chatId}`;
        const paired = await ctx.completePairing(code, {
          userConfig: { chatId },
          displayLabel,
        });
        const replyText = paired.ok
          ? `✅ Paired with Cito as @${paired.owner_user ?? "user"}`
          : `❌ Pairing failed: ${paired.error ?? "Invalid or expired code"}`;
        await callTelegramApi(
          botToken,
          "sendMessage",
          { chat_id: chatId, text: replyText },
          { apiBaseUrl, fetchFn: ctx.fetchFn },
        ).catch(() => {});
        processed += 1;
        continue;
      }

      const principal = ctx.resolvePrincipalByUserConfig("chatId", chatId);
      if (!principal) continue;

      const replyToMsgId = msg.reply_to_message?.message_id != null ? String(msg.reply_to_message.message_id) : null;
      const target = replyToMsgId
        ? ctx.resolveReplyTarget(principal.principal_id, { replyToExternalMsgId: replyToMsgId })
        : ctx.resolveReplyTarget(principal.principal_id, { maxAgeMs: 10 * 60_000 });

      if (!target) {
        await callTelegramApi(
          botToken,
          "sendMessage",
          {
            chat_id: chatId,
            text: "⚠️ No recent active notification to reply to. Swipe to reply to a specific Cito message.",
          },
          { apiBaseUrl, fetchFn: ctx.fetchFn },
        ).catch(() => {});
        processed += 1;
        continue;
      }

      const sent = await ctx.sendReplyFromHuman(principal.principal_id, {
        to_listener_id: target.from_listener_id,
        message: text,
        in_reply_to: target.signal_id,
      });

      const targetLabel = target.from_title || (target.from_user ? `@${target.from_user}` : target.from_listener_id);
      const confirmText = sent.ok
        ? `↩ Replied to ${sent.to_label || targetLabel}`
        : `❌ Reply failed: ${sent.error ?? "Target unavailable"}`;
      await callTelegramApi(
        botToken,
        "sendMessage",
        { chat_id: chatId, text: confirmText },
        { apiBaseUrl, fetchFn: ctx.fetchFn },
      ).catch(() => {});
      processed += 1;
    }

    await ctx.setServiceState({
      ...ctx.getServiceState(),
      update_offset: nextOffset,
    });
    return processed;
  }

  async startService(ctx: HumanChannelServiceContext): Promise<{ stop(): Promise<void> | void }> {
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const loop = async () => {
      if (stopped) return;
      try {
        await this.pollOnce(ctx);
      } catch {}
      if (!stopped) {
        timer = setTimeout(loop, 1500);
        timer.unref?.();
      }
    };
    timer = setTimeout(loop, 200);
    timer.unref?.();
    return {
      stop() {
        stopped = true;
        if (timer) clearTimeout(timer);
      },
    };
  }
}

export const telegramPlugin = new TelegramChannelPlugin();
export default telegramPlugin;
