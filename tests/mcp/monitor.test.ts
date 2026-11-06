import { afterEach, expect, test, vi } from "vitest";
import { setTimeout as delay } from "node:timers/promises";
import { fixture, call, waitFor } from "./support.js";
import { startNats } from "../shared/nats-server.js";
import { toCanonicalListenerId } from "../../src/shared/index.js";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => { vi.restoreAllMocks(); for (const close of cleanups.splice(0).reverse()) await close(); });

test("a failed reconnect KV read retries restoration and keeps monitoring later reconnect cycles", async () => {
  const f = await fixture(); cleanups.push(() => f.close());
  const enrolled = await call(f.service, "cito_listen", { about: "monitor recovery" }, f.pi.meta);
  const key = `default:${toCanonicalListenerId(enrolled.listener_id)}`, slot = f.service.slots.get("default")!, broker = await slot.network();
  const old = f.service.receivers.get(key)!;
  const reads = vi.spyOn(broker.kv, "get").mockRejectedValueOnce(new Error("transient restoration KV timeout"));
  await broker.nc.reconnect();
  await waitFor(() => reads.mock.calls.length >= 2 && slot.error === null && f.service.receivers.get(key) !== old && f.service.receivers.get(key)?.abort.signal.aborted === false);
  expect(slot.error).toBeNull(); expect(slot.status).toBe("connected");
  const recovered = f.service.receivers.get(key)!;
  await broker.nc.reconnect();
  await waitFor(() => f.service.receivers.get(key) !== recovered && f.service.receivers.get(key)?.abort.signal.aborted === false);
  expect(recovered.abort.signal.aborted).toBe(true);
  expect(await call(f.service, "cito_send", { to: enrolled.listener_id, message: "after second reconnect" })).toMatchObject({ status: "wake_accepted" });
}, 15000);

test("disconnect cancels a pending restoration retry", async () => {
  const f = await fixture(); cleanups.push(() => f.close());
  await call(f.service, "cito_listen", { about: "retry cancellation" }, f.pi.meta);
  const slot = f.service.slots.get("default")!, broker = await slot.network();
  const reads = vi.spyOn(broker.kv, "get").mockRejectedValue(new Error("KV still unavailable"));
  await broker.nc.reconnect(); await waitFor(() => reads.mock.calls.length > 0);
  await slot.disconnect(); const count = reads.mock.calls.length;
  await delay(350);
  expect(reads.mock.calls.length).toBe(count); expect(slot.status).toBe("disconnected");
});

test("an active connection's real NATS permission rejection emits auth_required without another tool call", async () => {
  const broker = await startNats(`no_auth_user: anonymous
  authorization {
    users: [{user: anonymous, permissions: {publish: {allow: [">"], deny: ["forbidden"]}, subscribe: ">"}}]
  }`);
  cleanups.push(() => broker.stop());
  const f = await fixture({ broker, observeBanner: true }); cleanups.push(() => f.close());
  const enrolled = await call(f.service, "cito_listen", { about: "terminal auth" }, f.pi.meta);
  const receiver = f.service.receivers.get(`default:${toCanonicalListenerId(enrolled.listener_id)}`)!;
  const slot = f.service.slots.get("default")!, nc = (await slot.network()).nc;
  const snapshots = vi.spyOn(f.service.ui!, "state");
  nc.publish("forbidden");
  await nc.flush().catch(() => {});
  await waitFor(() => snapshots.mock.calls.some(([hubs]) => hubs.some(hub => hub.status === "auth_required")));
  expect(slot.status).toBe("auth_required"); expect(slot.error).toContain("PERMISSIONS_VIOLATION");
  expect(receiver.abort.signal.aborted).toBe(true);
  expect(snapshots.mock.lastCall?.[0]).toMatchObject([{ name: "default", status: "auth_required", error: expect.stringContaining("PERMISSIONS_VIOLATION") }]);
  expect(snapshots.mock.lastCall?.[1]).toEqual([]);
});

test("unexpected connection closure emits disconnected and cancels active receivers", async () => {
  const f = await fixture({ observeBanner: true }); cleanups.push(() => f.close());
  const enrolled = await call(f.service, "cito_listen", { about: "unexpected closure" }, f.pi.meta);
  const receiver = f.service.receivers.get(`default:${toCanonicalListenerId(enrolled.listener_id)}`)!, slot = f.service.slots.get("default")!;
  const broker = await slot.network(), snapshots = vi.spyOn(f.service.ui!, "state");
  await broker.nc.close();
  await waitFor(() => snapshots.mock.lastCall?.[0][0]?.status === "disconnected");
  expect(slot.error).toBe("NATS connection closed unexpectedly");
  expect(receiver.abort.signal.aborted).toBe(true); expect(snapshots.mock.lastCall?.[1]).toEqual([]);
});
