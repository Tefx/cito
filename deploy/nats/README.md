# Local NATS deployment

These files run standard NATS JetStream. They do not implement a custom relay.
Ports are published on loopback only. Remote production access requires an
operator-selected trusted interface and TLS/network controls.

## No authentication (default)

```sh
docker compose -f deploy/nats/docker-compose.yml up -d nats
```

`CITO_AUTH_MODE=none` selects `nats-server.conf` with no authorization
block. Application clients provision KV/stream topology on startup.

## OIDC

Set the following environment variables before starting:

- `CITO_AUTH_MODE=oidc`
- `CITO_OIDC_ISSUER=https://id.example.com`
- `CITO_OIDC_CLIENT_ID=<registered-client-id>`
- `CITO_AUTH_CALLOUT_PASSWORD=<strong-service-password>`
- `CITO_AUTH_CALLOUT_ISSUER_SEED=<SA... Account seed>`
- `CITO_AUTH_CALLOUT_ISSUER=<matching A... public key>`

Optionally set both `CITO_AUTH_CALLOUT_XKEY_SEED=<SX...>` and its matching
public `CITO_AUTH_CALLOUT_XKEY=<X...>`. Compose automatically selects the
encrypted configuration when the public XKey is non-empty. Never commit seeds
or passwords. Account/XKey pairs can be generated with `nkeys.js`
(`createAccount()` / `createCurve()`) or NATS `nsc`; obtain the public key
with `getPublicKey()` and retain the seed securely.

```sh
docker compose -f deploy/nats/docker-compose.yml --profile oidc up -d --build
```

The `oidc` profile starts the stateless responder in addition to NATS.
Both `CITO_AUTH_MODE=oidc` and the profile are needed. Changing modes requires
recreating the NATS container. The responder restarts on transient startup
connection failures; configuration failures remain visible in its stderr.
The application account is `CITO`, the system account is `$SYS`, and only
the configured `callout-service` credential bypasses external authentication.

Native server use requires `CITO_NATS_STORE_DIR` set to an isolated writable
directory, plus the OIDC public-key/password variables above when applicable:

```sh
CITO_NATS_STORE_DIR=/path/to/data nats-server -c deploy/nats/nats-server.conf
bin/cito-auth-callout
```

## Client configuration

Clients can connect to neutral named hubs such as `personal` (no authentication)
and `work` (OIDC issuer `https://id.example.com`) simultaneously. Start
`cito daemon` once per user; its loopback MCP endpoint supports `2026-07-28`
and existing `2025-*` clients. Caller detection belongs to registered adapters,
not the MCP transport.

The V4 macOS client includes the `makeCitoStatusIcon` template vector and a
340pt Control-Center card menu with hub status pills and connection actions.
See the [client guide](../../README.md#macos-menu-bar-extra-cito-ui) for controls
and the [optional stdio channel bridge](../../README.md#optional-claude-stdio-channel-bridge)
for `CITO_CLAUDE_CHANNEL=1`. Native Claude Code lifecycle integration remains
**Proposed Design Suggestion Only — Not Yet Implemented**; see
[DESIGN.md §5](../../docs/DESIGN.md#5-proposed-design-for-claude-code-integration-design-suggestion-only-not-yet-implemented).
None of these client features changes the broker deployment or installs host hooks.

## Auth responder behavior and compatibility

The executable validates config before connecting. It discovers OIDC and
JWKS lazily on token verification with a 5000ms timeout per HTTP fetch.
Its in-memory key cache refreshes on unknown `kid` with a 60s cooldown;
concurrent cache misses share the refresh. Cache-miss fetch failure produces a
signed `AUTH_UNAVAILABLE` rejection, and usable cached keys remain available.
NATS reports all denied connections as `Authorization Violation`; callers
must not assume that broker error distinguishes transient provider outages.

Only RS256, RS384, RS512, PS256, ES256, ES384 and EdDSA tokens with matching issuer/audience, non-empty subject and
`exp > now - 30s` pass OIDC verification. NATS user claims retain the
original token expiry, so NATS itself can reject a token already expired
within that tolerance. Signed JWT v2 permissions cover only the declared
JetStream/KV, Cito and inbox subjects, including `$JS.ACK.>`.

SIGINT/SIGTERM stop intake, finish pending verifications, drain the connection
and clear keys. No host state, OIDC token files or business payloads are read.

**Account-seed limitation:** although the task text mentioned `SO...`,
official `nats-server 2.14.0 -t` rejects an Operator public key as
`auth_callout.issuer`: “Expected callout user to be a valid public account
nkey”. The responder therefore rejects SO seeds at startup with a descriptive
error and uses SA seeds. SA-only enforcement is the approved resolution for
this non-operator topology; an Operator-auth path is outside this deployment.

Tests run against an installed official NATS 2.14.0 binary and a local mock
OIDC HTTP server. Compose configuration is syntax-checked; a live OIDC
provider and Docker image deployment require separate operator verification.
