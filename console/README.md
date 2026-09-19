# NovBot center console

React + TypeScript + Vite UI for the open-source NovBot control center.

By default the browser talks to same-origin **`/v1`** (relative path; Vite proxies in dev). **Settings → API** may set an absolute `http://` / `https://` base URL and a Bearer token (stored in `localStorage`) for demos against a remote center.

## Prerequisites

- Node.js 20+
- A running `novbot-center` HTTP admin API (default `http://127.0.0.1:8080`)

## Install

```bash
cd console
npm install
```

## Development

```bash
# Optional: override center URL for the Vite proxy
# export NOVBOT_CENTER_URL=http://127.0.0.1:8080

npm run dev
```

Vite proxies `/v1` and `/health` to the center so the app can use relative `/v1/...` calls.

Open the printed local URL (HTTP by default).

### Optional: Vite HTTPS via mkcert

For local TLS matching production same-origin habits:

```bash
# once per machine
mkcert -install
mkdir -p .certs
mkcert -key-file .certs/localhost-key.pem -cert-file .certs/localhost.pem localhost 127.0.0.1 ::1
npm run dev
```

If `.certs/localhost-key.pem` and `.certs/localhost.pem` exist, Vite enables HTTPS automatically. Do not commit `.certs/`.

## Production / same-origin `/v1`

Serve the built static assets and reverse-proxy `/v1` to the center on the **same HTTPS origin**. Example with [Caddy](https://caddyserver.com/) + mkcert (or any ACME cert):

```bash
npm run build
# dist/ holds the static app
```

Example Caddyfile sketch (adjust host and upstream):

```caddyfile
console.example.local {
  tls internal   # or certificates from mkcert / Let's Encrypt
  handle /v1/* {
    reverse_proxy 127.0.0.1:8080
  }
  handle /health {
    reverse_proxy 127.0.0.1:8080
  }
  handle {
    root * ./dist
    try_files {path} /index.html
    file_server
  }
}
```

The SPA uses relative `/v1` — no API host configuration is required in the browser bundle.

## Scripts

| Script | Description |
|--------|-------------|
| `npm run dev` | Vite dev server + `/v1` proxy |
| `npm run build` | Typecheck + production build to `dist/` |
| `npm run preview` | Preview the production build |
| `npm run lint` | Oxlint |

## Routes

| Path | Page | API |
|------|------|-----|
| `/` | Overview aggregates | `GET /health`, `GET /v1/nodes`, `GET /v1/results` |
| `/nodes` | Nodes list | `GET /v1/nodes` |
| `/nodes/:nodeId/inventory` | Node inventory (read-only meta) | `GET /v1/nodes` + `GET /v1/nodes/:id/config` |
| `/nodes/:nodeId/config` | Config editor | `GET/PUT /v1/nodes/:id/config` |
| `/nodes/:nodeId/schedules` | Schedules editor | `GET/PUT /v1/nodes/:id/schedules` |
| `/nodes/:nodeId/dispatch` | Dispatch form | `POST /v1/nodes/:id/dispatch` |
| `/nodes/:nodeId/results` | Node-scoped results | `GET /v1/results?node_id=` |
| `/results` | Global results (+ optional node filter) | `GET /v1/results` |
| `/fleet/skill-groups` | Fleet skill-group push | `GET /v1/fleet/skill-groups`, `POST .../push`, `GET /v1/nodes` |
| `/settings/api` | API base URL + Bearer token | `localStorage`; `GET/POST /v1/tokens` |

Client: `src/api/client.ts` (+ `src/api/settings.ts`). Empty base URL → relative `/v1`; otherwise absolute `{base}/v1` with optional `Authorization: Bearer`.
