import { afterEach, expect, test, vi } from "vitest";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { CitoService } from "../../src/mcp/service.js";
import { registrationSchema } from "../../src/mcp/state.js";
import { encode } from "../../src/mcp/receiver.js";
import { toCanonicalListenerId } from "../../src/shared/index.js";
import { fixture, call, waitFor } from "./support.js";
import { signingKey, startOidc, browser } from "../auth-callout/oidc-fixture.js";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => { vi.restoreAllMocks(); for (const close of cleanups.splice(0).reverse()) await close(); });
async function setup() { const f = await fixture({ observeBanner: true }); cleanups.push(() => f.close()); return f; }

test("menu disconnect retains ACTIVE rows while removing live receivers, blocks every network call, and reconnect/login restore them", async () => {
  const f = await setup(), slot = f.service.slots.get("default")!;
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "disconnect", tag: "connected" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId), old = f.service.receivers.get(`default:${id}`)!;
  const states = vi.spyOn(f.service.ui!, "state");
  await f.service.handleUiAction({ op: "hub_action", hub: "default", action: "disconnect" });
  expect(slot.manuallyDisconnected).toBe(true); expect(slot.status).toBe("disconnected");
  expect(old.abort.signal.aborted).toBe(true); expect(f.service.state.listener(id)!.status).toBe("ACTIVE");
  expect(f.pi.badges.at(-1)).toBeUndefined(); expect(states.mock.lastCall?.[1]).toEqual([]);
  await expect(slot.network()).rejects.toMatchObject({ code: "NATS_UNAVAILABLE", message: "Hub 'default' is manually disconnected" });
  expect((await f.service.call("cito_find")).isError).toBe(true);
  for (const action of ["reconnect", "login"] as const) {
    await f.service.handleUiAction({ op: "hub_action", hub: "default", action });
    expect(slot.manuallyDisconnected).toBe(false); expect(slot.status).toBe("connected");
    expect(f.service.receivers.get(`default:${id}`)!.abort.signal.aborted).toBe(false);
    expect(states.mock.lastCall?.[1]).toMatchObject([{ listener_id: id }]);
  }
});

test("KV-first scope changes preserve owner/generation; probe and restore reconcile authoritative remote scope", async () => {
  const f = await setup(), slot = f.service.slots.get("default")!;
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "scope" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId), before = f.service.state.listener(id)!, broker = await slot.network();
  const update = broker.kv.update.bind(broker.kv);
  const barrier = vi.spyOn(broker.kv, "update").mockImplementationOnce(async (...args) => {
    expect(f.service.state.listener(id)!.scope).toBe("self");
    const revision = await update(...args);
    expect(f.service.state.listener(id)!.scope).toBe("self");
    return revision;
  });
  await f.service.handleUiAction({ op: "set_scope", hub: "default", listener_id: id, scope: "workspace" });
  barrier.mockRestore();
  expect(f.service.state.listener(id)).toMatchObject({ scope: "workspace", owner_token: before.owner_token, binding_generation: before.binding_generation });
  expect(registrationSchema.parse((await broker.kv.get(id))!.json()).scope).toBe("workspace");
  const failure = vi.spyOn(broker.kv, "update").mockRejectedValueOnce(new Error("KV offline"));
  await expect(f.service.setScope("default", id, "self")).rejects.toThrow("KV offline"); failure.mockRestore();
  expect(f.service.state.listener(id)!.scope).toBe("workspace");
  let entry = (await broker.kv.get(id))!;
  await broker.kv.update(id, encode({ ...registrationSchema.parse(entry.json()), scope: "self" }), entry.revision);
  await f.service.receivers.get(`default:${id}`)!.sweep();
  expect(f.service.state.listener(id)!.scope).toBe("self");
  entry = (await broker.kv.get(id))!;
  await broker.kv.update(id, encode({ ...registrationSchema.parse(entry.json()), scope: "workspace" }), entry.revision);
  const receiver = f.service.receivers.get(`default:${id}`)!; await receiver.stop();
  await slot.network();
  expect(f.service.state.listener(id)!.scope).toBe("workspace");
  expect(f.service.receivers.get(`default:${id}`)).not.toBe(receiver);
});

test("UI state obeys the live-receiver owner fence; unlisten and unlisten-all make pending signals passive; quit closes the real companion", async () => {
  const f = await setup();
  const enrolled = await call(f.service, "cito_listen", { about: "UI actions" }, f.pi.meta), id = toCanonicalListenerId(enrolled.listener_id);
  const states = vi.spyOn(f.service.ui!, "state");
  const receiver = f.service.receivers.get(`default:${id}`)!;
  f.service.state.run("UPDATE local_listeners SET owner_token = 'different' WHERE listener_id = ?", id);
  f.service.pushUiState(); expect(states.mock.lastCall?.[1]).toEqual([]);
  f.service.state.run("UPDATE local_listeners SET owner_token = ? WHERE listener_id = ?", receiver.owner, id);
  f.service.pushUiState(); expect(states.mock.lastCall?.[1]).toMatchObject([{ listener_id: id }]);
  await call(f.service, "cito_send", { to: id, message: "cancel via menu" });
  await f.service.handleUiAction({ op: "unlisten", hub: "default", listener_id: id });
  expect(f.service.state.listener(id)!.status).toBe("UNLISTENED");
  expect(f.service.state.signals("SELECT * FROM local_signals")[0]).toMatchObject({ state: "STORED_PASSIVE", held_reason: "unlistened_before_dispatch" });
  await call(f.service, "cito_listen", { about: "all" }, f.pi.meta);
  await f.service.handleUiAction({ op: "unlisten_all" });
  expect(f.service.state.db.prepare("SELECT COUNT(*) AS n FROM local_listeners WHERE status = 'ACTIVE'").get()!.n).toBe(0);
  const child = Reflect.get(f.service.ui!, "child");
  await f.service.handleUiAction({ op: "quit" }); expect(child.exitCode).toBe(0);
});

test("disconnect aborts interactive login and prevents a late connection; subsequent reconnect and disconnect actions serialize", async () => {
  const key = await signingKey("ES256", "cancel"), oidc = await startOidc([key.jwk]); cleanups.push(() => oidc.stop());
  let opened = false;
  const f = await fixture({ env: { CITO_AUTH_MODE: "oidc", CITO_OIDC_ISSUER: oidc.issuer, CITO_OIDC_CLIENT_ID: "cito" },
    service: { browserOpener: async () => { opened = true; await new Promise(() => {}); } } }); cleanups.push(() => f.close());
  const slot = f.service.slots.get("default")!;
  const pending = slot.network();
  const failure = expect(pending).rejects.toBeDefined();
  await waitFor(() => opened);
  await slot.disconnect(); await failure;
  expect(slot.connectedBroker()).toBeUndefined(); expect(slot.manuallyDisconnected).toBe(true);
  expect(slot.status).toBe("disconnected");
  // Both operations act on the same lifetime queue. Disconnect interrupts login.
  const login = slot.reconnect(true); const cancelled = expect(login).rejects.toBeDefined();
  await waitFor(() => slot.status === "connecting"); await slot.disconnect(); await cancelled;
  expect(slot.connectedBroker()).toBeUndefined(); expect(slot.status).toBe("disconnected");
});

test("HubSlot optional scopes and redirect ports cannot leak from the mirrored default hub", async () => {
  const f = await fixture(); cleanups.push(() => f.close());
  const config = { ...f.config, oidcScopes: ["openid", "private"], oidcRedirectPort: 9999,
    hubs: { ...f.config.hubs, secondary: { ...f.config.hubs.default!, name: "secondary" } } };
  const service = await CitoService.create(config); cleanups.push(() => service.close());
  expect(service.slots.get("secondary")!.config.oidcScopes).toBeUndefined();
  expect(service.slots.get("secondary")!.config.oidcRedirectPort).toBeUndefined();
});

test("non-interactive startup never opens a browser, and authenticating as a new principal fences previous listeners before restoration", async () => {
  const key = await signingKey("ES256", "principal"), oidc = await startOidc([key.jwk]); cleanups.push(() => oidc.stop());
  let opened = 0;
  const f = await fixture({ env: { CITO_AUTH_MODE: "oidc", CITO_OIDC_ISSUER: oidc.issuer, CITO_OIDC_CLIENT_ID: "cito" }, service: { browserOpener: async (url, signal) => { opened++; await browser(url, signal); } } }); cleanups.push(() => f.close());
  await f.service.connectBackground(); expect(opened).toBe(0); expect(f.service.slots.get("default")!.status).toBe("auth_required");
  oidc.state.tokenResponse = { id_token: await key.sign(oidc.claims()), refresh_token: "refresh" };
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "old account" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId), receiver = f.service.receivers.get(`default:${id}`)!;
  await f.service.handleUiAction({ op: "hub_action", hub: "default", action: "disconnect" });
  const cachePath = join(f.stateDir, "oidc-token-default.json"), cache = JSON.parse(await readFile(cachePath, "utf8"));
  cache.expires_at = 0; await writeFile(cachePath, JSON.stringify(cache));
  oidc.state.tokenResponse = { id_token: await key.sign(oidc.claims({ sub: "other-account", preferred_username: "bob" })) };
  await f.service.handleUiAction({ op: "hub_action", hub: "default", action: "reconnect" });
  expect(opened).toBe(1);
  expect(f.service.slots.get("default")!.principal?.owner_user).toBe("bob");
  expect(f.service.state.listener(id)!.status).toBe("FENCED_INACTIVE");
  expect(receiver.abort.signal.aborted).toBe(true);
  expect(f.service.slots.get("default")!.receivers.get(id)?.abort.signal.aborted).toBe(true);
});
