import { afterEach, expect, test, vi } from "vitest";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { CitoService } from "../../src/mcp/service.js";
import { discoverAllAvailableSessions } from "../../src/adapters/index.js";
import { registrationSchema } from "../../src/mcp/state.js";
import { encode } from "../../src/mcp/receiver.js";
import { loadConfig, toCanonicalListenerId } from "../../src/shared/index.js";
import { fixture, call, waitFor, freePort } from "./support.js";
import { signingKey, startOidc, browser } from "../auth-callout/oidc-fixture.js";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => { vi.restoreAllMocks(); for (const close of cleanups.splice(0).reverse()) await close(); });
async function setup() { const f = await fixture({ observeBanner: true }); cleanups.push(() => f.close()); return f; }

test("menu disconnect retains ACTIVE rows while removing live receivers, blocks every network call, and reconnect/login restore them", async () => {
  const f = await setup(), slot = f.service.slots.get("default")!;
  const { listener_id: publicId } = await call(f.service, "cito_listen", { about: "disconnect", tag: "connected" }, f.pi.meta);
  const id = toCanonicalListenerId(publicId), old = f.service.receivers.get(`default:${id}`)!;
  const states = vi.spyOn(f.service.ui!, "state");
  f.service.pushUiState();
  expect(states.mock.lastCall?.[1]).toMatchObject([{ listener_id: id, about: "disconnect", branch: f.service.state.listener(id)!.branch }]);
  slot.error = "CitoError: Hub disconnected";
  await f.service.handleUiAction({ op: "hub_action", hub: "default", action: "disconnect" });
  expect(slot.manuallyDisconnected).toBe(true); expect(slot.status).toBe("disconnected"); expect(slot.error).toBeNull();
  expect(old.abort.signal.aborted).toBe(true); expect(f.service.state.listener(id)!.status).toBe("ACTIVE");
  expect(f.pi.badges.at(-1)).toBeUndefined(); expect(states.mock.lastCall?.[1]).toEqual([]);
  await expect(slot.network()).rejects.toMatchObject({ code: "NATS_UNAVAILABLE", message: "Hub 'default' is manually disconnected" });
  expect((await f.service.call("cito_find")).isError).toBe(true);
  for (const action of ["reconnect", "login"] as const) {
    await f.service.handleUiAction({ op: "hub_action", hub: "default", action });
    expect(slot.manuallyDisconnected).toBe(false); expect(slot.status).toBe("connected"); expect(slot.error).toBeNull();
    expect(f.service.receivers.get(`default:${id}`)!.abort.signal.aborted).toBe(false);
    expect(states.mock.lastCall?.[1]).toMatchObject([{ listener_id: id, about: "disconnect" }]);
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
  expect(slot.status).toBe("disconnected"); expect(slot.error).toBeNull();
  // Both operations act on the same lifetime queue. Disconnect interrupts login.
  const login = slot.reconnect(true); const cancelled = expect(login).rejects.toBeDefined();
  await waitFor(() => slot.status === "connecting"); await slot.disconnect(); await cancelled;
  expect(slot.connectedBroker()).toBeUndefined(); expect(slot.status).toBe("disconnected"); expect(slot.error).toBeNull();
});

test("HubSlot optional scopes and redirect ports cannot leak from the mirrored default hub", async () => {
  const f = await fixture(); cleanups.push(() => f.close());
  const key = await signingKey("ES256", "scopes"), oidc = await startOidc([key.jwk]); cleanups.push(() => oidc.stop());
  oidc.state.scopesSupported = ["openid", "email"];
  oidc.state.tokenResponse = { id_token: await key.sign(oidc.claims()) };
  const path = join(f.stateDir, "config.json"), port = await freePort();
  const hub = { natsUrl: f.broker.url, authMode: "oidc", oidcIssuer: oidc.issuer, oidcClientId: "cito" };
  await writeFile(path, JSON.stringify({ default_hub: "work", hubs: {
    work: { ...hub, oidcScopes: ["openid", "custom"], oidcRedirectPort: port }, cloud: hub,
  } }));
  const config = loadConfig({ ...f.env, CITO_CONFIG_PATH: path });
  expect(config.oidcScopes).toEqual(["openid", "custom"]);
  expect(config.hubs.cloud).not.toHaveProperty("oidcScopes");
  const requestedScopes: string[] = [];
  const service = await CitoService.create(config, { browserOpener: async (url, signal) => {
    requestedScopes.push(new URL(url).searchParams.get("scope")!); await browser(url, signal);
  } }); cleanups.push(() => service.close());
  expect(service.slots.get("cloud")!.config.oidcScopes).toBeUndefined();
  expect(service.slots.get("cloud")!.config.oidcRedirectPort).toBeUndefined();
  await service.slots.get("work")!.network();
  await service.slots.get("cloud")!.network();
  expect(requestedScopes).toEqual(["openid custom", "openid email"]);
  expect(service.slots.get("cloud")!.principal?.owner_user).toBe("alice");
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
  // Switch Account (action: "login") while already connected forces interactive OIDC re-login and fences bob's listener
  const { listener_id: bobPublicId } = await call(f.service, "cito_listen", { about: "bob session" }, f.pi.meta);
  const bobId = toCanonicalListenerId(bobPublicId), bobReceiver = f.service.receivers.get(`default:${bobId}`)!;
  oidc.state.tokenResponse = { id_token: await key.sign(oidc.claims({ sub: "third-account", preferred_username: "carol" })) };
  await f.service.handleUiAction({ op: "hub_action", hub: "default", action: "login" });
  expect(opened).toBe(2);
  expect(f.service.slots.get("default")!.status).toBe("connected");
  expect(f.service.slots.get("default")!.error).toBeNull();
  expect(f.service.slots.get("default")!.principal?.owner_user).toBe("carol");
  expect(f.service.state.listener(bobId)!.status).toBe("FENCED_INACTIVE");
  expect(bobReceiver.abort.signal.aborted).toBe(true);
});

test("refresh_available and listen_session enroll discovered sessions, synchronously filter active listeners, and fence changed sessions", async () => {
  const f = await setup();
  const states = vi.spyOn(f.service.ui!, "state");
  await f.service.handleUiAction({ op: "refresh_available" });
  expect(states.mock.lastCall?.[2]).toMatchObject([
    { harness: "pi", session_id: "pi-session-1", endpoint_pid: f.pi.meta.pi_pid, title: "Pi receiver" },
  ]);
  // 1-click listen_session enrolls the candidate and synchronously removes it from available in pushUiState()
  await f.service.handleUiAction({
    op: "listen_session",
    harness: "pi",
    session_id: "pi-session-1",
    endpoint_pid: f.pi.meta.pi_pid,
    cwd: f.stateDir,
    title: "Multi-line\nSession Title",
  });
  expect(states.mock.lastCall?.[1]).toMatchObject([
    { harness: "pi", session_id: "pi-session-1", about: "Multi-line Session Title", scope: "self" },
  ]);
  expect(states.mock.lastCall?.[2]).toEqual([]);
  // Unlistening restores the live session to available
  await f.service.handleUiAction({ op: "unlisten_all" });
  await waitFor(() => (states.mock.lastCall?.[2]?.length ?? 0) === 1);
  expect(states.mock.lastCall?.[2]).toMatchObject([{ harness: "pi", session_id: "pi-session-1" }]);
  // Identity fencing: if the session switches before listen_session runs, it fails with SESSION_NOT_OPEN and refreshes available
  f.pi.switchSession("switched-session-id");
  await expect(f.service.handleUiAction({
    op: "listen_session",
    harness: "pi",
    session_id: "pi-session-1",
    endpoint_pid: f.pi.meta.pi_pid,
    cwd: f.stateDir,
    title: "Old Session",
  })).rejects.toMatchObject({ code: "SESSION_NOT_OPEN", message: "Session changed since discovery" });
  expect(f.service.state.db.prepare("SELECT COUNT(*) AS n FROM local_listeners WHERE status = 'ACTIVE'").get()!.n).toBe(0);
  await waitFor(() => states.mock.lastCall?.[2]?.[0]?.session_id === "switched-session-id");
});

test("CitoService populates my_remote_listeners and evaluates query_directory across HubDirectoryIndex with prefix tokens and facets", async () => {
  const f = await setup();
  const slot = f.service.slots.get("default")!;
  const broker = await slot.network();
  const principal = await slot.identity();
  const states = vi.spyOn(f.service.ui!, "state");
  const directories = vi.spyOn(f.service.ui!, "directory");

  // Enroll 1 real local session on this host, plus seed 1 remote self-principal session and 1 team workspace peer
  const { listener_id: localPublicId } = await call(f.service, "cito_listen", { about: "Local Pi session", tag: "local" }, f.pi.meta);
  const localId = toCanonicalListenerId(localPublicId);
  const aliceRemoteReg = {
    schema_version: 1 as const,
    listener_id: "c_aliceremote2",
    public_id: "cito-aliceremote2",
    tag: "cito",
    principal_id: principal.principal_id,
    owner_user: principal.owner_user,
    host_id: "11111111-1111-4111-8111-111111111111",
    host_name: "alice-mini.local",
    harness: "pi",
    session_id: "remote-s1",
    cwd: "/Users/alice/cito",
    repo: "acme/cito",
    branch: "main",
    title: "Alice Remote Pi",
    about: "Remote build check",
    scope: "self" as const,
    state: "ACTIVE" as const,
    binding_generation: 1,
    owner_token: "11111111-1111-4111-8111-222222222222",
    lease_ttl_seconds: 60 as const,
  };
  const bobPeerReg = {
    ...aliceRemoteReg,
    listener_id: "c_bobworkspace",
    public_id: "billing-bobworkspace",
    tag: "billing",
    principal_id: "none#bob",
    owner_user: "bob",
    host_id: "22222222-2222-4222-8222-222222222222",
    host_name: "bob-studio",
    harness: "codex",
    session_id: "bob-s1",
    repo: "acme/billing",
    branch: "feat/payments",
    title: "Billing Service",
    about: "Payment webhook handler",
    scope: "workspace" as const,
  };
  await broker.kv.create(aliceRemoteReg.listener_id, encode(aliceRemoteReg));
  await broker.kv.create(bobPeerReg.listener_id, encode(bobPeerReg));

  await f.service.handleUiAction({ op: "open_directory" });
  expect(states.mock.lastCall?.[3]).toHaveLength(1);
  expect(states.mock.lastCall?.[3]).toMatchObject([{ listener_id: "c_aliceremote2", is_self_principal: true }]);
  expect(states.mock.lastCall?.[4]).toBe(3);
  expect(directories.mock.lastCall).toEqual([
    expect.arrayContaining([
      expect.objectContaining({ listener_id: localId, host_id: f.service.hostId, is_self_principal: true }),
      expect.objectContaining({ listener_id: "c_aliceremote2", is_self_principal: true }),
      expect.objectContaining({ listener_id: "c_bobworkspace", is_self_principal: false }),
    ]),
    3,
    3,
  ]);
  // Self principal sessions sort before team peers by default
  expect(directories.mock.lastCall?.[0]?.slice(0, 2).every(p => p.is_self_principal)).toBe(true);
  expect(directories.mock.lastCall?.[0]?.[2]?.listener_id).toBe("c_bobworkspace");

  // Prefix token filtering: @bob repo:billing host:studio is:codex #bobwork
  await f.service.handleUiAction({ op: "query_directory", query: "@bob repo:billing host:studio is:codex #bobwork", scope: "all", harness: "all" });
  expect(directories.mock.lastCall?.[0]?.map(p => p.listener_id)).toEqual(["c_bobworkspace"]);
  expect(directories.mock.lastCall?.[1]).toBe(1);
  expect(directories.mock.lastCall?.[2]).toBe(3);

  // Scope facet "mine" restricts to self principal across both local and remote hosts
  await f.service.handleUiAction({ op: "query_directory", query: "", scope: "mine", harness: "all" });
  expect(directories.mock.lastCall?.[0]?.map(p => p.listener_id)).toHaveLength(2);
  expect(new Set(directories.mock.lastCall?.[0]?.map(p => p.listener_id))).toEqual(new Set([localId, "c_aliceremote2"]));
  expect(directories.mock.lastCall?.[1]).toBe(2);
  expect(directories.mock.lastCall?.[2]).toBe(3);
  await f.service.handleUiAction({ op: "close_directory" });
});

test("multi-hub candidate retention (listening_hubs) and atomic switch_hub migration with failure rollback", async () => {
  const base = await fixture(); cleanups.push(() => base.close());
  const configPath = join(base.stateDir, "multi-hub.json");
  await writeFile(configPath, JSON.stringify({
    default_hub: "default",
    hubs: {
      default: { natsUrl: base.broker.url, authMode: "none", user: "alice" },
      work: { natsUrl: base.broker.url, authMode: "none", user: "bob" },
    },
  }));
  const config = loadConfig({ ...base.env, CITO_CONFIG_PATH: configPath });
  const service = await CitoService.create(config, base.serviceOptions); cleanups.push(() => service.close());
  await service.startUi();
  const states = vi.spyOn(service.ui!, "state");

  // Legacy hub-less activeListeners rows exclude matching candidate even in multi-hub config
  const legacyDiscovered = await discoverAllAvailableSessions(config, [{ harness: "pi", session_id: "pi-session-1" }]);
  expect(legacyDiscovered).toEqual([]);

  // Enroll on default hub via listen_session with explicit hub & scope
  await service.handleUiAction({
    op: "listen_session",
    harness: "pi",
    session_id: "pi-session-1",
    endpoint_pid: base.pi.meta.pi_pid,
    cwd: base.stateDir,
    title: "Multi-Hub Session",
    hub: "default",
    scope: "workspace",
  });
  await waitFor(() => states.mock.lastCall?.[2]?.[0]?.listening_hubs?.includes("default") === true);
  expect(states.mock.lastCall?.[1]).toMatchObject([
    { hub: "default", harness: "pi", session_id: "pi-session-1", scope: "workspace", about: "Multi-Hub Session" },
  ]);
  expect(states.mock.lastCall?.[2]).toMatchObject([
    { harness: "pi", session_id: "pi-session-1", listening_hubs: ["default"] },
  ]);

  const defaultListenerId = states.mock.lastCall![1]![0]!.listener_id;
  // Set custom tag via cito_listen to verify switch_hub inherits about, scope, and tag
  await call(service, "cito_listen", { hub: "default", about: "Tagged Session", scope: "workspace", tag: "v9tag" }, base.pi.meta);

  // Failure rollback: disconnect work hub so enrollment on work fails; default hub listener must remain active!
  await service.handleUiAction({ op: "hub_action", hub: "work", action: "disconnect" });
  await service.handleUiAction({ op: "switch_hub", from_hub: "default", to_hub: "work", listener_id: defaultListenerId });
  expect(service.slots.get("work")!.error).toContain("Hub 'work' is manually disconnected");
  expect(service.state.listener(defaultListenerId, "default")?.status).toBe("ACTIVE");
  expect(service.receivers.get(`default:${defaultListenerId}`)?.abort.signal.aborted).toBe(false);

  // Reconnect work hub and switch_hub from default -> work atomically
  await service.handleUiAction({ op: "hub_action", hub: "work", action: "reconnect" });
  await service.handleUiAction({ op: "switch_hub", from_hub: "default", to_hub: "work", listener_id: defaultListenerId });
  expect(service.state.listener(defaultListenerId, "default")?.status).toBe("UNLISTENED");
  await waitFor(() => states.mock.lastCall?.[2]?.[0]?.listening_hubs?.[0] === "work");
  const workListenerId = states.mock.lastCall![1]![0]!.listener_id;
  expect(service.state.listener(workListenerId, "work")).toMatchObject({
    status: "ACTIVE",
    about: "Tagged Session",
    scope: "workspace",
    tag: "v9tag",
  });
  expect(states.mock.lastCall?.[1]).toMatchObject([{ hub: "work", listener_id: workListenerId, scope: "workspace", about: "Tagged Session" }]);

  // Enrolling on default as well (so both hubs are active) excludes the candidate from available
  await service.handleUiAction({
    op: "listen_session",
    harness: "pi",
    session_id: "pi-session-1",
    endpoint_pid: base.pi.meta.pi_pid,
    cwd: base.stateDir,
    title: "Tagged Session",
    hub: "default",
  });
  expect(states.mock.lastCall?.[2]).toEqual([]);

  // switch_hub when to_hub is already active skips duplicate listen() and unlistens from_hub
  const listenSpy = vi.spyOn(service as unknown as { listen: (...args: unknown[]) => Promise<unknown> }, "listen");
  await service.handleUiAction({ op: "switch_hub", from_hub: "work", to_hub: "default", listener_id: workListenerId });
  expect(listenSpy).not.toHaveBeenCalled();
  expect(service.state.listener(workListenerId, "work")?.status).toBe("UNLISTENED");
  expect(service.state.listener(defaultListenerId, "default")?.status).toBe("ACTIVE");
});
