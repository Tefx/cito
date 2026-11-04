# V1 host adapters

Import from `src/adapters/index.ts`. Receiver tools pass the request's `_meta`
object directly to `resolveCodexCaller(meta, config)` or
`resolvePiCaller(meta, config)`. `resolveCaller(harness, meta, config)` dispatches
an explicitly selected V1 harness. These functions never guess from focus or
recency. Outbound `cito_find` and `cito_send` must route by target identity and
must not require receiver resolution.

Both callers return `harness`, `session_id`, `cwd`, `title`, `repo`, and `branch`.
Pi also returns `pi_pid`, `session_name`, `is_streaming`, `bridge_epoch`, and
`human_event_count`. Probe Pi with `identifyPi(pi_pid, config)`; the listener
owner remains responsible for single-flight probes, persisted epoch deltas,
fencing, and liveness policy.

## Pi bridge

Load `src/pi-extension/cito-bridge.ts` directly using Pi's extension mechanism.
It imports only Node built-ins and can be copied outside this repository with
no package installation. Set `CITO_STATE_DIR` in both the Pi host and MCP
processes to select the same state directory. The bridge starts listening on
`session_start`, closes on `session_shutdown`, and also unlinks its socket on
normal process exit. An uncatchable process termination cannot run cleanup.

`setPiStatus(endpoint, badge, config, signal?)` updates or clears the footer.
`deliverPiWake(endpoint, batchId, content, count, config, signal?)` submits an
already-frozen card; `wakePi(endpoint, batchId, target, signals, config, signal?)`
formats one before submission. Delivery returns `delivered` and `deduplicated`.
Both functions make one attempt. Abort prevents further submission or closes an
in-flight socket; once the host has accepted a request, abort cannot undo it.

`PiBridgeError` preserves the bridge's exact `bridgeError` reason. In particular,
`SESSION_MISMATCH` maps to the shared MCP `SESSION_NOT_OPEN` code while retaining
`bridgeError === "SESSION_MISMATCH"` for dispatcher revocation. Transport,
malformed-response, and deadline failures use `PI_BRIDGE_UNAVAILABLE`; lack of
any Pi ancestor or hint uses `BINDING_REQUIRED`.

## Codex Desktop

`readCodexThread(threadId, config, { allowArchived? })` reads `state_5.sqlite`
without creating or modifying it. Display titles prefer trimmed `name`, then
trimmed `title`, then the cwd basename; schemas without `name` also work.
Archived threads are rejected by default. Inbox/unlisten callers can explicitly
set `allowArchived: true` to read a known closed session without an IPC probe.
Missing threads still fail. `assertCodexThreadOpen` verifies Desktop liveness.
`CodexSessionError.definitive` distinguishes absent/archived threads and explicit
no-owner responses from transient SQLite/socket/protocol failures, allowing the
MCP liveness sweep to apply its three-strike rule. On a missing/refused IPC socket,
`codexDesktopRunning(signal)` checks for the ChatGPT.app process within the probe budget.
`resolveCodexCaller` performs the default read and probe, rejecting
archived/missing threads and unavailable Desktop owners for registration.

IPC requests use `type: "request"`, `version: 1`, and a UUID `requestId`.
Initialize sends `params: { clientType: "cli" }` and requires `result.clientId`.
Discovery sends that `sourceClientId` and
`params: { hostId: "local", conversationId: threadId, threadId }`.
A discovery `resultType: "success"` establishes liveness, including results
`{ status: "alive" }` and `{}` with no owner ID. Explicit `status: "no-client-found"`,
`ownerClientId: null`, or `ownerClientId: ""` reports closure even on success;
any non-success result also fails with `SESSION_NOT_OPEN`. These requests follow
DESIGN.md §7.3, retaining `threadId` alongside `conversationId` for compatibility.
Local Unix socket tests verify the wire contract; live Desktop compatibility
remains unverified.

`queueCodexWake(threadId, card, { codexBin?, signal? })` and
`triggerCodexWake(threadId, { openBin?, signal? })` expose separate effects.
The dispatcher must hold its listener lock across its authorization check and
**each** call. The adapter neither acquires that lock nor retries. Queue timeout
has an unknown insertion outcome; the dispatcher owns the documented
`queue_written` recovery policy and must not blindly repeat queue insertion.
Executable failures retain Node's `execFile` error fields for that decision.

`formatWakeCard(target, signals)` preserves signal order, renders all display
fields with angle-bracket escaping, quotes attribute data, and lists the exact
acknowledgment IDs. Message sanitization belongs at admission, before rendering.

## Verification

```sh
npm run typecheck
npm test -- tests/adapters/
```

Tests use temporary state directories, SQLite databases, Unix sockets, Git
repositories, and executable fixtures. They do not open real Codex/Pi sessions
or invoke `/usr/bin/open`. The optional installed-host test loads a standalone
copy through the actual Pi 1.5.0 loader and event runner, uses an in-memory
session and controlled send callback, and checks host custom-entry persistence
and both shutdown and process-exit cleanup. Set `CITO_PI_PACKAGE_DIR` to an
installed Pi 1.5.0 package directory; absent an override, it checks the standard
`~/.pi/agent/install/releases/1.5.0/node_modules/@earendil-works/pi-coding-agent`
location and skips only that installed-host test if unavailable. No model or
live Desktop service is contacted.
