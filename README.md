# NovBot

Open-source **node daemon + API-only control center** for configurable host inspection and thin compliance checks.

**Product of NovHub Innovation Laboratory** (under TrendOverflow).  
Homepage (placeholder): https://novhub.hk

## License

Apache-2.0. See [LICENSE](LICENSE).

## Architecture (MVP)

| Component | Role |
|-----------|------|
| `novbot-center` | MySQL-backed config authority; gRPC `Control.Session` bidi stream; HTTP admin API |
| `novbot-node` | Long-running daemon; pull-on-boot config into memory; scheduled / dispatched probes; report + retry spool; single-file egress |
| `novbot-core` | Shared JSON specs, probes, skills/MCP tools, schedules, retry spool |
| `novbot-proto` | `novbot.v1.Control` protobuf |
| `console/` | OSS center console (React + TypeScript + Vite); browser calls same-origin `/v1` |

**Out of OSS MVP:** EE report packs, product design docs in-repo.

## Center console

See [`console/`](console/) for the web UI scaffold (`npm install` / `npm run dev` with Vite proxy to center).

## Prerequisites

- Rust stable (1.85+)
- `protoc` (protobuf compiler)
- **MySQL 8** (bring-your-own is primary). Optional: `docker compose up -d` for a local instance.

## Quick start

### 1. MySQL

```bash
# Optional local MySQL (image mysql:8.4; caching_sha2 is default — do not pass
# --default-authentication-plugin, which is invalid on 8.4)
docker compose up -d

export NOVBOT_DATABASE_URL='mysql://novbot:novbot@127.0.0.1:3306/novbot'
```

Center applies migrations under `crates/novbot-center/migrations/` on startup.
JSON-shaped fields are stored as `LONGTEXT` so sqlx can decode them as `String` on MySQL 8.4.

### 2. Build

```bash
cargo build --workspace
cargo test --workspace
```

### 3. Run center

```bash
export NOVBOT_DATABASE_URL='mysql://novbot:novbot@127.0.0.1:3306/novbot'
# optional: export NOVBOT_BOOTSTRAP_TOKEN=secret
# optional stub license: export NOVBOT_LICENSE_KEY=dev-license
cargo run -p novbot-center -- \
  --database-url "$NOVBOT_DATABASE_URL" \
  --grpc-addr 0.0.0.0:50051 \
  --http-addr 0.0.0.0:8080
```

### 4. Seed node config (HTTP)

```bash
curl -sS -X PUT http://127.0.0.1:8080/v1/nodes/demo-1/config \
  -H 'content-type: application/json' \
  -d '{
    "specs": [
      {"id":"cpu","kind":"cpu"},
      {"id":"mem","kind":"memory"},
      {"id":"root-exists","kind":"compliance_path","params":{"path":"/","must_exist":true}},
      {"id":"sshd","kind":"compliance_sshd","params":{"permit_root_login":"no","password_authentication":"no"}},
      {"id":"ports","kind":"compliance_listening_ports","params":{"max_sample":16}},
      {"id":"ww","kind":"compliance_world_writable","params":{"root":"/etc","max_findings":20}},
      {"id":"ntp","kind":"compliance_ntp"},
      {"id":"reboot","kind":"compliance_reboot_required"},
      {"id":"host-skill","kind":"skill","params":{"skill":"host_info"}},
      {"id":"echo-mcp","kind":"mcp_tool","params":{"tool":"echo","arguments":{"text":"hello"}}}
    ],
    "schedules": [
      {"spec_id":"cpu","interval_secs":30,"enabled":true},
      {"spec_id":"mem","cron":"*/5 * * * *","enabled":true},
      {"spec_id":"host-skill","interval_secs":120,"enabled":true}
    ]
  }'
```

### 5. Run node

```bash
cargo run -p novbot-node -- \
  --center-grpc http://127.0.0.1:50051 \
  --node-id demo-1 \
  --data-dir ./data/demo-1
```

On success the node writes `./data/demo-1/last_result.json` (single-file OSS egress).

### 6. Dispatch a skill / MCP tool (M5)

```bash
# Live push if node Session is connected; otherwise queued until next heartbeat/pull
curl -sS -X POST http://127.0.0.1:8080/v1/nodes/demo-1/dispatch \
  -H 'content-type: application/json' \
  -d '{"spec_id":"host-skill"}'

curl -sS -X POST http://127.0.0.1:8080/v1/nodes/demo-1/dispatch \
  -H 'content-type: application/json' \
  -d '{"spec_id":"echo-mcp","params":{"arguments":{"text":"from-dispatch"}}}'
```

Built-in skills/tools: `host_info`, `echo`, `env_get` — see `GET /v1/skills`.

## Spec kinds

| Kind | Role |
|------|------|
| `cpu` / `memory` / `disk` | Host metrics |
| `compliance_path` | Path exists / must_exist |
| `compliance_sshd` | `PermitRootLogin` / `PasswordAuthentication` in sshd_config |
| `compliance_listening_ports` | Sample LISTEN ports from `/proc/net/tcp{,6}` |
| `compliance_world_writable` | Bounded walk for world-writable paths (default `/etc`) |
| `compliance_ntp` | `timedatectl` / `chronyc` sync check |
| `compliance_reboot_required` | `/var/run/reboot-required` marker |
| `exec` | Bounded shell command |
| `skill` | In-process skill (`params.skill`) |
| `mcp_tool` | MCP-style tool (`params.tool` + `params.arguments`) |

## Schedules (M11)

Center-authored; node holds in memory; restored on boot via `PullConfig`.

| Field | Meaning |
|-------|---------|
| `interval_secs` | Fire when elapsed since last run ≥ N seconds |
| `cron` | 5-field UTC cron: `minute hour dom month dow` (`*`, `N`, `*/N`, lists, ranges) |
| `enabled` | Default true |

At least one of `interval_secs` or `cron` must be set for a schedule to fire. Results are reported like any other probe.

## License gate stub (M7)

Same center process. Without a license:

- `GET /v1/ee/reports` and `GET /v1/ee/reports/:id` return **402** (`license_required`)

Enable with `NOVBOT_LICENSE_KEY` or `PUT /v1/license` `{"key":"..."}` (stub accepts any non-empty key). OSS still does not ship EE report packs.

## HTTP API

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Liveness |
| GET | `/v1/nodes` | List registered nodes |
| GET/PUT | `/v1/nodes/:id/config` | Get/put specs (+ optional schedules) |
| GET/PUT | `/v1/nodes/:id/schedules` | Get/put schedules |
| POST | `/v1/nodes/:id/dispatch` | DispatchCommand (skill / mcp_tool / probe) |
| GET | `/v1/results?node_id=&limit=` | Recent probe results |
| GET/PUT | `/v1/license` | License stub status / set key |
| GET | `/v1/skills` | List built-in skills/tools |
| GET | `/v1/ee/reports` | EE reports (license-gated stub) |

## gRPC

`Control.Session` bi-directional stream: Register, Heartbeat, PullConfig, ReportResult, Ack; server may PushConfig / Dispatch / PushSchedule.

## Repositories

- Public OSS: `trendoverflow/novbot`
- Commercial enterprise artifacts: `trendoverflow/novbot-enterprise` (proprietary)
