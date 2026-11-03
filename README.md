# Cito

**Cito** (Latin *citō*: "to summon into action / swiftly") is a notification-only cross-harness, cross-machine, multi-user wake and signaling service for coding agents.

## Scope (V1)

- **Active V1 Targets**:
  - **`cito-hub`**: HTTP + WebSocket signaling relay with SQLite persistence, lease management, wake-quota and pairwise circuit-breaker enforcement, and optional standard OIDC JWT authentication (compatible with GCP Casdoor).
  - **`cito-mcp`**: Unified stdio MCP server providing `cito_listen`, `cito_unlisten`, `cito_find`, `cito_send`, and `cito_inbox`.
  - **Codex Desktop App (`ChatGPT.app`) Adapter**: Caller binding via `_meta["x-codex-turn-metadata"].thread_id`, tab liveness check via `~/.codex/ipc/ipc.sock`, queue injection via `codex queue`, silent wake via `codex://threads/wake/<thread_id>`, and banner click-to-jump via `codex://threads/<thread_id>`.
  - **Pi (`1.5.0`) Adapter**: Local Unix socket bridge extension (`src/pi-extension/cito-bridge.ts`) keyed by Pi process PID (`~/.cito/sockets/pi-pid-<pid>.sock`) calling `pi.sendMessage(..., { triggerTurn, deliverAs: "followUp" })`.
  - **macOS Native HUD Banner (`cito-banner`)**: Single-instance on-demand `NSPanel` HUD notification banner compiled with system `/usr/bin/swiftc`.
- **Deferred (TODO / Proposed Only)**:
  - **Claude Code Adapter**: Documented in `docs/DESIGN.md` §9 strictly as a proposed design (unverified in local runtime because Claude Code is not installed on the target host). Excluded from V1 implementation and the Vectl plan.

See [`docs/DESIGN.md`](docs/DESIGN.md) for the complete architecture and protocol specification.
