import { afterEach, expect, test } from "vitest";
import { join } from "node:path";
import { isolatedState, unixServer } from "./support.js";
import { fixture, call, payload } from "../mcp/support.js";
import { loadConfig, toCanonicalListenerId } from "../../src/shared/index.js";
import { extractCallerPidRoots, resolveCaller, SocketBridgeAdapter, getAdapter } from "../../src/adapters/index.js";
import type { ListenerRow } from "../../src/mcp/state.js";

const cleanups: (() => Promise<unknown>)[] = [];
afterEach(async () => { for (const cleanup of cleanups.splice(0).reverse()) await cleanup(); });
test("PID extraction validates explicit claims and uses nested roots without appending the parent", () => {
  expect(extractCallerPidRoots({ endpoint_pid: 11, caller_pid: 11, pi_pid: 11 })).toEqual([11]);
  expect(extractCallerPidRoots({ gateway: { parent_pid: 22, child: { endpoint_pid: 33 }, ignored: 0 }, other: { pi_pid: 44, caller_pid: 33 } })).toEqual([22, 33, 44]);
  expect(extractCallerPidRoots({ gateway: { parent_pid: -1, caller_pid: 0 } })).toEqual([process.ppid]);
  for (const pid of [0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, "11"]) expect(() => extractCallerPidRoots({ endpoint_pid: pid })).toThrow(expect.objectContaining({ code: "PI_BRIDGE_UNAVAILABLE" }));
  expect(() => extractCallerPidRoots({ endpoint_pid: 11, pi_pid: 12 })).toThrow(expect.objectContaining({ code: "INVALID_INPUT" }));
});
test("a nested explicit root cannot accidentally bind to a bridge on process.ppid", async () => {
  const f = await isolatedState(); cleanups.push(() => f.cleanup());
  const server = await unixServer(join(f.stateDir, `sockets/aider-pid-${process.ppid}.sock`), socket => socket.once("data", () => socket.end(JSON.stringify({
    ok: true, endpoint_pid: process.ppid, session_id: "parent", session_name: "Parent", cwd: f.root,
  }) + "\n"))); cleanups.push(() => server.stop());
  const config = loadConfig(f.env);
  expect((await resolveCaller({}, config)).harness).toBe("aider");
  await expect(resolveCaller({ gateway: { parent_pid: 2147483647 } }, config)).rejects.toMatchObject({ code: "BINDING_REQUIRED" });
});
test("socket probes distinguish matching, switched, definitive closure, dead process and transient failure", async () => {
  const f = await isolatedState(); cleanups.push(() => f.cleanup()); const config = loadConfig(f.env);
  let response: object = { ok: true, endpoint_pid: process.pid, session_id: "s", session_name: "S", cwd: f.root };
  const server = await unixServer(join(f.stateDir, `sockets/aider-pid-${process.pid}.sock`), socket => socket.once("data", () => socket.end(JSON.stringify(response) + "\n")));
  cleanups.push(() => server.stop());
  const row = { endpoint_pid: process.pid, session_id: "s" } as ListenerRow, adapter = new SocketBridgeAdapter("aider");
  expect(await adapter.probe(row, config, new AbortController().signal)).toEqual({ status: "alive" });
  response = { ok: true, endpoint_pid: process.pid, session_id: "other", session_name: "S", cwd: f.root };
  expect(await adapter.probe(row, config, new AbortController().signal)).toEqual({ status: "switched" });
  response = { ok: false, error: "SESSION_NOT_OPEN" };
  expect(await adapter.probe(row, config, new AbortController().signal)).toEqual({ status: "closed_definitive" });
  response = { ok: false, error: "TEMPORARY" };
  expect((await adapter.probe(row, config, new AbortController().signal)).status).toBe("transient_error");
  expect((await adapter.probe({ ...row, endpoint_pid: 2147483647 }, config, new AbortController().signal)).status).toBe("closed_definitive");
  expect((await new SocketBridgeAdapter("unreachable").probe(row, config, new AbortController().signal)).status).toBe("transient_error");
  expect(getAdapter("invalid:id")).toBeUndefined();
});
test("offline PID fallback accepts one retained pair across hubs/statuses and rejects zero or multiple pairs", async () => {
  const f = await fixture(); cleanups.push(() => f.close());
  const enrolled = await call(f.service, "cito_listen", { about: "cached" }, f.pi.meta), id = toCanonicalListenerId(enrolled.listener_id);
  f.service.state.run("UPDATE local_listeners SET endpoint_pid = 2147483647, status = 'UNLISTENED'");
  f.service.state.run("INSERT INTO local_listeners SELECT 'other-hub', listener_id, public_id, tag, principal_id, owner_user, host_id, host_name, harness, session_id, endpoint_pid, cwd, repo, branch, title, about, scope, 'FENCED_INACTIVE', 9, owner_token, persisted_seq, consecutive_probe_failures, replay_gap_detected, replay_gap_range, dispatcher_token, lease_until_ms FROM local_listeners WHERE hub = 'default'");
  const offlineMeta = { endpoint_pid: 2147483647 };
  expect(await call(f.service, "cito_inbox", {}, offlineMeta)).toMatchObject({ listener_id: enrolled.listener_id, signals: [] });
  expect(payload(await f.service.call("cito_inbox", {}, { endpoint_pid: 2147483646 }))).toMatchObject({ error: { code: "PI_BRIDGE_UNAVAILABLE" } });
  f.service.state.run("UPDATE local_listeners SET session_id = 'another' WHERE hub = 'other-hub'");
  expect(payload(await f.service.call("cito_inbox", {}, offlineMeta))).toMatchObject({ error: { code: "PI_BRIDGE_UNAVAILABLE" } });
  expect(f.service.state.listener(id)!.status).toBe("UNLISTENED");
});
