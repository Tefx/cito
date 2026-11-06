// purpose: scheduling barriers around real daemon UI startup and tracked network operations.
// usage: node --import tsx tests/mcp/fixtures/daemon-lifecycle.ts daemon (with an IPC channel).
// effects: runs the real daemon against the caller's isolated state/ports; reports lifecycle observations.
// requires: isolated CITO_STATE_DIR, CITO_CODEX_HOME, CITO_DAEMON_PORT; parent releases gates and stops the child.
import { CitoService } from "../../../src/mcp/service.js";
import { UiBridge } from "../../../src/ui/index.js";
import { main } from "../../../src/mcp/cli.js";

let holdOperation = false, releaseOperation: (() => void) | undefined, releaseUi: (() => void) | undefined;
process.on("message", message => {
  if (message === "hold_operation") { holdOperation = true; process.send?.({ event: "operation_armed" }); }
  if (message === "release_operation") releaseOperation?.();
  if (message === "release_ui") releaseUi?.();
});
const startUi = UiBridge.prototype.start;
UiBridge.prototype.start = async function () {
  if (process.env.CITO_TEST_HOLD_UI === "1") {
    await new Promise<void>(resolve => { releaseUi = resolve; process.send?.({ event: "ui_starting" }); });
  }
  await startUi.call(this);
  process.send?.({ event: "ui_ready", child_spawned: Reflect.get(this, "child") !== undefined });
};
const create = CitoService.create;
CitoService.create = async (...args) => {
  const service = await create(...args), slot = service.slots.get(service.config.defaultHub)!;
  const network = slot.network.bind(slot);
  slot.network = async (...options) => {
    const broker = await network(...options);
    if (holdOperation && options[0]?.interactive !== false) {
      holdOperation = false;
      await new Promise<void>(resolve => { releaseOperation = resolve; process.send?.({ event: "operation_held" }); });
    }
    return broker;
  };
  const close = service.close.bind(service);
  let closing = false;
  service.close = async () => {
    if (!closing) { closing = true; process.send?.({ event: "service_closing" }); }
    await close();
    process.send?.({ event: "service_closed" });
    setImmediate(() => { if (process.connected) process.disconnect(); });
  };
  return service;
};
try { await main(); }
catch (error) { console.error(error); process.exitCode = 1; if (process.connected) process.disconnect(); }
