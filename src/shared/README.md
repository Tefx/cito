# Shared V1 contracts

Import the public helpers from `src/shared/index.ts`. The project uses Node
>=22.13 (excluding Node 23/25 for the test runner), ESM, strict TypeScript,
Zod 4, and the built-in `node:sqlite` module. No native SQLite npm addon is
required. The dependency lock also includes the MCP SDK, jose, tsx, NATS JWT
v2 and NKey/XKey libraries for downstream steps.

## Configuration and identity

- `loadConfig(env?)` validates startup configuration without touching disk.
  It returns camelCase properties (`natsUrl`, `authMode`, `oidcIssuer`,
  `oidcClientId`, `user`, `stateDir`, `codexHome`, `codexBin`, `hostId`, `hostName`).
- Defaults: `nats://127.0.0.1:4222`, `none`, `~/.cito`, `~/.codex`,
  and `/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex`.
  `CITO_CODEX_BIN` overrides the executable. OIDC requires an HTTPS issuer
  (loopback HTTP is permitted for tests) and a non-empty client ID.
  `user` preserves `CITO_USER` or the OS username and must match
  `^[a-zA-Z0-9._-]{1,64}$`; invalid values fail at startup.
- `getHostId(config)` returns an explicit `CITO_HOST_ID` or atomically creates
  a persisted lowercase UUIDv4 in `stateDir/host-id`. It uses exclusive
  hard-link publication so concurrent initializers see one complete identity.
  State directory mode is 0700; the file mode is 0600. Invalid persisted IDs
  fail rather than silently changing listener identity.
- `localPrincipal(config.user)` lowercases the configured username, validates
  `^[a-z0-9._-]{1,64}$`, and yields `principal_id="none#" + owner_user`.
  Without an argument it selects `CITO_USER` or the OS username.
- `oidcPrincipal(verifiedClaims)` yields normalized `iss#sub`; display handle
  selects trimmed non-blank `preferred_username`, then the trimmed non-empty
  email local-part before `@`, then `sub`. It lowercases the handle, replaces
  non-`[a-z0-9._-]` characters with `_`, and truncates to 64 characters.
  `name` is not a fallback. Callers must verify JWTs first.
- `listenerId(SessionKey)` derives a keyless deterministic
  `c_[a-z2-7]{12}` ID. `senderEndpointKey` distinguishes bound sessions
  from the `unbound|default` namespace.

Configuration and identity follow DESIGN.md §2.1/§3.1.1. Consumers must use
these shared functions consistently so listener IDs, self-send checks and
principal scope comparisons share the same canonical identity.

## Validation and results

`sanitizeMessage` strips exactly the specified ASCII ranges and validates
1–500 Unicode codepoints after trimming. `escapeWakeCard` escapes angle
brackets at render time; it must cover every interpolated card field.
The exact strip ranges omit CR (0x0D), which is therefore preserved internally.

Public input schemas are `listenInputSchema`, `unlistenInputSchema`,
`findInputSchema`, `sendInputSchema`, and `inboxInputSchema`.
`parseInput` converts schema errors to `CitoError` and reports unsupported
harnesses as `UNSUPPORTED_HARNESS`. V1 accepts only Codex and Pi.
`successResult` and `errorResult` emit the canonical MCP text envelope.
All 15 error codes and retryability values live in `ERROR_RETRYABLE`.

## NATS and broker time

`connectNats(options)` selects TCP (`nats`) or WebSocket (`nats.ws` with
Node's native WebSocket). Both expose the NATS v2 API. Do not mix transports
in a server list.

`provisionNats(nc)` returns `{ js, jsm, kv }`, creates the file-backed
`CITO_SIGNALS` stream and `cito_listeners` KV bucket, and verifies the
required settings on existing resources. It never resets existing data or
silently repairs incompatible retention/history policies.

`brokerNow(jsm, stream?)` requires a broker-generated StreamInfo timestamp.
`brokerAgeMs`, `evaluateLease`, and `localExpirationMs` implement the
60-second visibility / 30-minute retention boundaries and conservative
local expiry mapping without trusting the sender's local clock.

## Tests for downstream steps

Run `npm ci` once. `npm test` discovers all `tests/**/*.test.ts` and
`*.spec.ts` without enumerating files. Directory filters work directly:

```sh
npm test -- tests/shared/ tests/auth-callout/
npm test -- tests/adapters/
npm test -- tests/banner/
npm run typecheck
npm run build
```

Reuse `tests/shared/nats-server.ts`:

```ts
const broker = await startNats();
try {
  const nc = await connectNats({ servers: broker.url });
  try {
    const { js, jsm, kv } = await provisionNats(nc);
    // Exercise the actual official JetStream/KV APIs.
  } finally { await nc.close(); }
} finally { await broker.stop(); }
```

The dev dependency `@eplightning/nats-server@2.14.0` distributes the official
NATS binary as platform-specific npm optional dependencies. Tests use the
installed binary, never a network download or Docker. Keep optional
dependencies enabled at install time; `NATS_SERVER_BINARY_PATH` can select
an existing compatible binary on other hosts. `startNats` selects random
loopback ports, creates isolated temporary broker storage, and removes it on
`stop()`. Its `env` supplies isolated `CITO_STATE_DIR` and
`CITO_CODEX_HOME` values for subprocess tests. Always close clients first.
It also accepts `{ configFile: absolutePath }` and an environment map for
deployment-config tests, and returns `wsUrl` when a WebSocket listener exists.
