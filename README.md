# Cito

**Cito** (Latin *citō*: "to summon into action / swiftly") is a notification-only cross-harness, cross-machine, multi-user wake and signaling service for coding agents, powered by **NATS (JetStream + KV)**.

## Scope (V1)

- **Active V1 Targets**:
  - **NATS Relay & Optional OIDC Auth Callout (`deploy/nats/` & `src/auth-callout/`)**: Uses standard `nats-server` (JetStream `CITO_SIGNALS` stream for 30m durable replay/deduplication + `cito_listeners` KV bucket for discovery/leases) and an optional lightweight OIDC Auth Callout service (compatible with GCP Casdoor).
  - **`cito-mcp`**: Unified stdio MCP server providing `cito_listen`, `cito_unlisten`, `cito_find`, `cito_send`, and `cito_inbox`, with local SQLite state, recipient-side anti-loop wake quota & pairwise circuit breaker, two-stage crash-safe batch dispatch, and OIDC PKCE Just-in-Time browser login.
  - **Codex Desktop App (`ChatGPT.app`) Adapter**: Caller binding via `_meta["x-codex-turn-metadata"].thread_id`, tab liveness check via `~/.codex/ipc/ipc.sock`, queue injection via `codex queue`, silent wake via `codex://threads/wake/<thread_id>`, and banner click-to-jump via `codex://threads/<thread_id>`.
  - **Pi (`1.5.0`) Adapter**: Local Unix socket bridge extension (`src/pi-extension/cito-bridge.ts`) keyed by Pi process PID (`~/.cito/sockets/pi-pid-<pid>.sock`) calling `pi.sendMessage(..., { triggerTurn, deliverAs: "followUp" })`.
  - **macOS Native HUD Banner (`cito-banner`)**: Single-instance on-demand `NSPanel` HUD notification banner (`src/macos/cito-banner.swift`) compiled with system `/usr/bin/swiftc`.
- **Deferred (TODO / Proposed Only)**:
  - **Claude Code Adapter**: Documented in `docs/DESIGN.md` §9 strictly as a proposed design (unverified in local runtime because Claude Code is not installed on the target host). Excluded from V1 implementation and the Vectl plan.

See [`docs/DESIGN.md`](docs/DESIGN.md) for the complete architecture and protocol specification.
