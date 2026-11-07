# Cito Hub Deployment (`cito hub`)

`cito hub` runs the native Node.js HTTP/SSE + SQLite WAL signaling server (`HubEngine`) with zero external broker dependencies.

## 1. Standalone CLI

```sh
# Trusted network / Tailscale mesh (auth=none):
cito hub --port 4291 --bind 127.0.0.1 --auth none --data-dir /var/lib/cito-hub

# Pre-shared bearer token (auth=token):
cito hub --port 4291 --bind 0.0.0.0 --auth token --token "<strong-secret-token>" --data-dir /var/lib/cito-hub

# Multi-user OIDC with Server-Side Row-Level Security (auth=oidc):
cito hub --port 4291 --bind 0.0.0.0 --auth oidc --issuer https://id.example.com --client-id <client-id> --data-dir /var/lib/cito-hub
```

## 2. Single-Container Docker Compose

```sh
# No-auth (loopback or Tailscale interface):
docker compose -f deploy/hub/docker-compose.yml up -d --build

# Token mode:
CITO_AUTH_MODE=token CITO_HUB_TOKEN="<strong-secret-token>" \
  docker compose -f deploy/hub/docker-compose.yml up -d --build

# OIDC mode:
CITO_AUTH_MODE=oidc CITO_OIDC_ISSUER="https://id.example.com" CITO_OIDC_CLIENT_ID="<client-id>" \
  docker compose -f deploy/hub/docker-compose.yml up -d --build
```

## 3. Client Onboarding (`cito setup`)

When `cito hub` starts, it prints the ready-to-copy `cito setup` command to `stderr`:

- **Trusted network (`auth=none`)**:
  ```sh
  cito setup --hub tailnet --url http://<host>:4291
  ```
- **Bearer token (`auth=token`)**:
  ```sh
  cito setup --hub tailnet --url http://<host>:4291 --auth token --token <token>
  ```
- **Team OIDC (`auth=oidc`)**:
  ```sh
  cito setup --hub team --url https://cito.example.com --auth oidc --issuer https://id.example.com --client-id <client-id> --default
  ```
