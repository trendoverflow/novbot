# NovBot runnable demo pack

English-only demo instructions for a local, **non-production** run of MySQL + center + node (+ optional console).

> **Security:** Values in `.env.example` are **DEMO-ONLY**. Copy to `demo/.env` (gitignored). Never commit secrets. Prefer leaving `NOVBOT_API_TOKEN` set so `/v1` requires Bearer auth.

## Prerequisites

- Docker / Docker Compose
- Rust stable (edition matching repo), `protoc`
- Node.js 20+ (console only)
- Repo at a revision that includes console P0+P1 (or newer `main`)

## Quick start

```bash
cp demo/.env.example demo/.env
# edit DEMO-ONLY passwords / tokens if desired
./demo/up.sh

# terminal A — center (inherits NOVBOT_API_TOKEN from demo/.env)
set -a; source demo/.env; set +a
cargo run -p novbot-center -- \
  --database-url "$NOVBOT_DATABASE_URL" \
  --grpc-addr "$NOVBOT_GRPC_ADDR" \
  --http-addr "$NOVBOT_HTTP_ADDR"

# terminal B — node
set -a; source demo/.env; set +a
cargo run -p novbot-node -- \
  --center-grpc "http://127.0.0.1:${NOVBOT_GRPC_PORT}" \
  --node-id demo-1 \
  --data-dir ./data/demo-1

# terminal C — smoke
./demo/smoke.sh
# optional fleet push: SMOKE_FLEET=1 ./demo/smoke.sh

./demo/down.sh          # keep volume
./demo/down.sh --volumes
```

Environment reference (also in `.env.example`):

| Variable | Role |
|----------|------|
| `NOVBOT_DATABASE_URL` | Center MySQL URL |
| `NOVBOT_API_TOKEN` | Bearer gate for `/v1` (recommended ON) |
| `NOVBOT_BOOTSTRAP_TOKEN` | Optional gRPC Register token |
| `NOVBOT_HTTP_ADDR` / `NOVBOT_GRPC_ADDR` | Listen addresses |
| `MYSQL_*` / `MYSQL_HOST_PORT` | Compose MySQL (DEMO-ONLY) |

Compose: root `docker-compose.yml` pins **`mysql:8.4`** and substitutes `${MYSQL_*}` (no hardcoded prod secrets). Overlay `demo/docker-compose.yml` attaches `env_file: .env`.

---

## Product demo steps (D1–D8)

| ID | Step | How |
|----|------|-----|
| **D1** | Prepare env | `cp demo/.env.example demo/.env`; confirm `NOVBOT_API_TOKEN` is set |
| **D2** | Start MySQL | `./demo/up.sh` (compose + `mysql:8.4`) |
| **D3** | Quality gates | `cargo test --workspace`; `cd console && npm run build` |
| **D4** | Start center | `cargo run -p novbot-center` with `NOVBOT_DATABASE_URL` + token |
| **D5** | Auth + health | `GET /health` → 200; `GET /v1/nodes` without Bearer → **401**; with Bearer → 200 |
| **D6** | Seed + node | `PUT /v1/nodes/demo-1/config` (see root README); run `novbot-node` |
| **D7** | Dispatch / fleet | `POST /v1/nodes/demo-1/dispatch` and/or `POST /v1/fleet/skill-groups/push` |
| **D8** | Console | `cd console && npm run dev`; Settings → paste Bearer; open Nodes / Fleet |

---

## Smoke map (S1–S8)

| ID | Check | Command / expect |
|----|-------|------------------|
| **S1** | Workspace tests | `cargo test --workspace` |
| **S2** | Console production build | `cd console && npm run build` |
| **S3** | Compose up | `./demo/up.sh` |
| **S4** | Health open | `GET /health` → 200 |
| **S5** | Unauth denied | `GET /v1/nodes` (no Bearer) → **401** when token set |
| **S6** | Auth OK | `GET /v1/nodes` + `Authorization: Bearer …` → 200 |
| **S7** | Optional fleet | `SMOKE_FLEET=1 ./demo/smoke.sh` |
| **S8** | Tear down | `./demo/down.sh` |

`./demo/smoke.sh` covers S4–S6 (and S7 when `SMOKE_FLEET=1`). Set `SMOKE_RUN_QUALITY=1` to also run S1–S2 from the smoke script.

---

## Security notes

1. **Do not commit `demo/.env`** — root `.gitignore` ignores `.env` / `.env.*` and keeps `!.env.example` / `!**/.env.example`.
2. **Auth default:** `NOVBOT_API_TOKEN` is **optional in code today** (empty = open `/v1`). **Demo profile must set it** so smoke can assert 401. Recommendation: treat demo as auth-default-on; consider a future `NOVBOT_API_TOKEN_REQUIRED=1` or deny-by-default for non-dev profiles.
3. **`/health` is intentionally open**; only `/v1/*` is gated.
4. Compose password defaults in `docker-compose.yml` (`root` / `novbot`) are **local demo fallbacks** when env is unset — override via `demo/.env`.
5. Console stores Bearer in **browser localStorage** — fine for demo; not a server-side session store.
6. CORS on center is currently permissive — acceptable for local demo only.

---

## Record commit SHA

After a green smoke on the revision you are validating:

```bash
git rev-parse HEAD
git rev-parse --short HEAD
```

Paste the full SHA into the acceptance / test report.

---

## Layout

```
demo/
  .env.example      # committed template (no real secrets)
  docker-compose.yml
  up.sh / down.sh / smoke.sh
  README.md         # this file
docker-compose.yml  # mysql:8.4 + ${MYSQL_*} substitution
```
