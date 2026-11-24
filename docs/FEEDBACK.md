# Cito Dogfooding & Field Feedback Ledger

This ledger records field observations, their user-visible effects, and the changes that address them. Phase IDs identify implementation work, not product releases.

## How to Submit Feedback

Find a repository maintainer session with `cito_find` and send a report with `cito_send`, or add an entry below. Include the date, observed behavior, and affected operation. Use vendor-neutral examples such as `personal`, `work`, and `alice`.

## Open Feedback Items

None pending triage.

## Resolved Feedback Items

| ID | Reported | Observation | Resolution | Status |
|---|---|---|---|---|
| **FB-01** | 2026-11-22 | Unnamed sessions in one repository all advertised its directory name as their description. | Discovery keeps the title separate from `about`, derives a short description from the native user prompt, and retains a meaningful description when restoring a listener. | `RESOLVED (v61)` |
| **FB-02** | 2026-11-22 | Wake-card text touched the top and bottom of the card. | The renderer adds a full-width background line above and below the content without changing the card colors. | `RESOLVED (v61)` |
| **FB-03** | 2026-11-23 | After an in-place app rebuild, a same-version daemon could keep serving from the removed executable, breaking inbound connections. | After publishing the signed bundle, setup stops the running daemon for that state directory and starts its replacement from the published helper. An offline installation stays offline. | `RESOLVED (v61)` |
| **FB-04** | 2026-11-23 | A peer closing the connection before the HTTP response looked like a truncated or oversized header. | HTTP parsing reports an empty initial read as a peer closure and retains separate diagnostics for malformed or incomplete headers. | `RESOLVED (v61)` |
| **FB-05** | 2026-11-23 | A profile with only a remote Hub lost same-host connectivity when that Hub went offline. | Remote Hub configuration retains the built-in `local://` Hub without changing an existing remote default. An explicit local opt-out survives subsequent default changes. | `RESOLVED (v61)` |
| **FB-06** | 2026-11-23 | Generic action receipts were hard to scan, and recipients tried to acknowledge already delivered cards. | The session bridge formats supported control-plane calls and results as cards, retains detailed diagnostics, and explains automatic acknowledgment on delivery. | `RESOLVED (v61)` |
| **FB-07** | 2026-11-24 | A short per-address connect timeout could expire before the first TCP retransmission, even with time left in the request. | A single resolved address gets up to 1.5 seconds to connect, still bounded by the overall request deadline. | `RESOLVED (v61)` |
| **FB-08** | 2026-11-24 | A probe during extension reload could register a temporary process-based address. | The bridge retains bound session identity across reload and reports that it is not ready before the first real session identity arrives. | `RESOLVED (v61)` |
| **FB-09** | 2026-11-24 | A test build overwrote the tracked UI binary even when none of its inputs were newer. | Non-release builds reuse the binary when its modification time is at least that of every Swift source and the build script. If a checkout makes the inputs newer, tests use a private build in Cargo's output directory. Only release builds stage the signed UI in `bin/`. | `RESOLVED (v61)` |
| **FB-10** | 2026-11-24 | Urgent signals accepted mid-turn could wait behind ordinary follow-up messages after acknowledgment. | Wake deliveries use steering at the next available turn boundary; non-waking messages remain follow-ups. | `RESOLVED (v61)` |
| **FB-11** | 2026-11-24 | A wake card lacked the signal identifier needed to correlate a reply after automatic acknowledgment. | The card pairs the reply address with the incoming signal ID, scoped to its Hub when needed, so a reply does not require another inbox read. | `RESOLVED (v61)` |
| **FB-12** | 2026-11-24 | Generic tool descriptions made control-plane tools difficult to find among unrelated messaging tools. | All six MCP descriptions name the tool and the Cito control plane, with distinct terms for discovery, registration, signaling, and acknowledgment. | `RESOLVED (v61)` |
| **FB-13** | 2026-11-24 | Automatic listeners inherited obsolete scopes after the Hub profile or authentication mode changed. | Automatic registration follows `defaultScope` and then the authentication-mode default, while retaining an explicit or manually chosen scope. | `RESOLVED (v61)` |
| **FB-14** | 2026-11-24 | Multi-Hub discovery returned duplicate listeners and UI-only action flags. | Unqualified `cito_find` groups a listener's reachable Hubs, prefers the default Hub's row, and omits UI action flags from the MCP result. | `RESOLVED (v61)` |
| **FB-15** | 2026-11-24 | A remote outage prevented an unqualified send to an otherwise reachable same-host listener. | Before publication, a transient default-Hub transport failure can use the connected local Hub for an active matching listener. Explicit Hub choices and uncertain publish outcomes are not rerouted. | `RESOLVED (v61)` |

### Earlier resolutions

| ID | Date | Source / Context | Category | Observation & Root Cause | Resolution | Status |
|---|---|---|---|---|---|---|
| **FB-R1** | 2026-11-22 | Multi-host setup (`host_hub` enabled on host Mac + CLI `cito setup --hub` pointing to own shared Hub URL) | Hub Slot Authority & UI Stability (`DSE-1`) | Adding a client Hub profile on the same Mac that hosts that shared Hub (`host_hub`) caused `"local"` and the self-pointing HTTP slot to overwrite each other's `binding_generation` and `owner_token` every ~500ms (`ACTIVE <-> FENCED_INACTIVE`), making the menu bar UI flicker between `listeners` and `available`. | Closed in **Phase `v58` (`ec42cc6`, `DSE-1`)**: unified `is_own_host_hub_url` and `validate_hub_endpoints` across all write admission boundaries (`paste_hub_invite`, `setup_save_hub`, `set_host_hub`, CLI `cito setup`) and skip self-loop/duplicate-endpoint slots in `reload_slots`. | `RESOLVED (v58)` |
| **FB-R2** | 2026-11-22 | Long-running tool calls in Pi sessions under `autoListen` | Harness Probe Equivalence (`DSE-2`) | `plugins/pi/native.rs` used `socket_response_or_cached` in `discover_sessions` but bypassed it in `probe`. When a busy Pi session timed out on `pi-pid-<pid>.sock` for 3 consecutive probes, `apply_probe` revoked it to `UNLISTENED (hard_exit)` and cleared its status bar badge, after which `discover_sessions` immediately re-enrolled it from `pi/endpoints/<pid>.json`. | Closed in **Phase `v58` (`ec42cc6`, `DSE-2`)**: unified `probe` and `discover_sessions` timeout cache fallback in `plugins/pi/native.rs`. | `RESOLVED (v58)` |
| **FB-R3** | 2026-11-22 | In-place binary upgrade while existing Pi sessions hold older `cito-native mcp-stdio` child processes | Upgrade Handover (`DSE-3`, `DSE-4`) | `handover_stale_daemon` required exact version equality (`running == CITO_VERSION`), causing an older `mcp-stdio` process in an un-reloaded session to `SIGTERM` a newly upgraded daemon and time out waiting for the old version. | Closed in **Phase `v58` (`ec42cc6`, `DSE-3`, `DSE-4`)**: monotonic same-major SemVer satisfaction (`running >= required`) in `handover_stale_daemon` / `ensure_daemon`, plus automatic daemon handover at the end of `cito setup` / `--build-app`. | `RESOLVED (v58)` |
