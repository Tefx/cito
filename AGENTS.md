<!-- VECTL:AGENTS:BEGIN -->
## Plan Tracking (vectl)

vectl tracks this repo's implementation plan as a structured `plan.yaml`:
what to do next, who claimed it, and what counts as done (with verification evidence).

Full guide: `vectl_guide` (CLI fallback: `vectl guide`)
Quick view: `vectl_status` (CLI fallback: `vectl status`)

### MCP vs CLI
- Source of truth: `plan.yaml` (channel-agnostic).
- **Always prefer MCP tools** (`vectl_status`, `vectl_claim`, etc.) when available.
- CLI fallback priority: `uv run vectl` > `vectl` > `uvx vectl`.
- Evidence requirements are identical across MCP and CLI.

### Claim-time Guidance
- `vectl claim` may emit a bounded Guidance block delimited by:
  - `--- VECTL:GUIDANCE:BEGIN ---`
  - `--- VECTL:GUIDANCE:END ---`
- For automation/CI: use `vectl claim --no-guidance` to keep stdout clean.

### plan.yaml — Managed File (DO NOT EDIT DIRECTLY)

`plan.yaml` is exclusively owned by vectl. Direct edits (Edit, Write, sed, or
any file tool) **will** corrupt plan state — vectl performs CAS writes, lock
recalculation, and schema validation on every save, none of which run on direct
edits.

**To modify plan state, ONLY use:**
- MCP (preferred): `vectl_claim`, `vectl_complete`, `vectl_mutate`, etc.
- CLI (fallback): `uv run vectl claim`, `vectl claim`, or `uvx vectl claim`, etc.

If a vectl command fails, report the error — do **not** edit `plan.yaml`
directly as a workaround. Use `vectl guide stuck` for troubleshooting.

### Rules
- One claimed step at a time.
- Evidence is mandatory when completing (commands run + outputs + gaps).
- Spec uncertainty: leave `# SPEC QUESTION: ...` in code, do not guess.

### Step ID Uniqueness
**Step IDs must be globally unique across ALL phases.**
- Example: `auth.login` and `api.login` are different step IDs.
- Example: Using just `login` in two phases creates a duplicate — not allowed.
- If you have legacy duplicate step IDs, use `vectl migrate-step-id --dry-run`
  to preview and `--yes` to repair.

### For Architects / Planners
- **Design Mode**: Run `vectl_guide` (CLI fallback: `vectl guide --on planning`).
- **Ambiguity = Failure**: Workers will hallucinate if steps are vague.
- **Constraint Tools**:
  - `--evidence-template`: Force workers to provide specific proof.
  - `--refs`: Pin specific files (e.g., `src/auth.py`) to worker context.
<!-- VECTL:AGENTS:END -->

## Language & Neutral Examples Policy

- **English-Only UI/UX, Code, and Documentation**: All user-facing UI/UX surfaces (menu bar labels, buttons, tooltips, HUD banners, in-chat wake cards, status badges), error messages, source comments, and repository documentation (`README.md`, `docs/**`) MUST be written in English, even when conversation with the user is in Chinese. (Unicode/CJK strings may appear only in targeted character-width or slug-sanitization test inputs).
- **Zero Company- or Environment-Specific Names**: Never hardcode or use internal company, hub, identity-provider, project, or colleague names anywhere in source code, UI, configuration defaults, tests, or documentation. Use vendor-neutral generic examples (`personal`, `work`, `cloud`, `alice`, `bob`, `acme/service`, `https://id.example.com`).

## Versioning, Compatibility & Release Governance

1. **Three Orthogonal Version Dimensions**:
   - **Product Release Version (`SemVer 2.0.0`, `vMAJOR.MINOR.PATCH`)**: Single source of truth in `package.json` `"version"` and exported as `CITO_VERSION` from `src/shared/index.ts`, paired with an annotated Git tag `vX.Y.Z` (e.g., `v0.10.0`).
   - **Internal Plan Phase (`v1`, `v2`, …)**: Tracked in `plan.yaml` for Vectl execution history only; never exposed as a runtime or wire version.
   - **Local SQLite Schema Version (`PRAGMA user_version`)**: Integer schema version in `src/mcp/state.ts`, incremented only on incompatible local table DDL changes.
2. **SemVer Increment Rules**:
   - **`PATCH` (`0.10.0 -> 0.10.1`)**: Backward-compatible bug fixes, UI contrast/layout refinements, packaging fixes, documentation updates, or internal refactors with no new MCP tool parameters, CLI flags, UI IPC actions, or NATS schema fields.
   - **`MINOR` (`0.10.x -> 0.11.0`)**: Backward-compatible feature additions, new optional MCP tool parameters or CLI flags, new `Daemon <-> UI` IPC fields/actions, or new optional NATS KV/signal fields.
   - **`MAJOR` (`0.x -> 1.0.0 -> 2.0.0`)**: Incompatible changes to NATS subjects/required schema fields, MCP tool signatures, or `~/.cito/config.json` format.
3. **Single Version Source & Automatic Stale-Daemon Handover**:
   - `CITO_VERSION` in `src/shared/index.ts` MUST equal `package.json` `"version"` (enforced by automated test) and be used by MCP `Server` metadata (`src/mcp/server.ts`), `GET /health` (`{ ok: true, pid, stateDir, version: CITO_VERSION }`), and `cito --version` / `cito -v`.
   - When `ensureDaemon(config)` (`src/mcp/cli.ts`) probes `GET /health` and finds a running daemon for `config.stateDir` whose `version !== CITO_VERSION` (including legacy daemons omitting `version`), it MUST send `SIGTERM` to that stale daemon PID, wait for shutdown, and spawn the current `cito-daemon` binary so upgrades take effect automatically without manual `kill`.
4. **Release & Pinning Discipline**:
   - Before tagging a release `vX.Y.Z`, update `package.json`, `package-lock.json`, and `CITO_VERSION`, verify `npm run typecheck && npm run compile && npm test` pass 100%, and create an annotated Git tag `git tag -a vX.Y.Z -m "Release vX.Y.Z: ..."`.
   - Consumer MCP configurations and server deployments SHOULD pin immutable semantic tags (`github:Tefx/cito#v0.10.0`) rather than raw commit hashes.
