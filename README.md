# NovBot

Open-source **node daemon + API-only control center** for configurable host inspection and thin compliance checks.

**Product of NovHub Innovation Laboratory** (under TrendOverflow).  
Homepage (placeholder): https://novhub.hk

## License

Apache-2.0. See [LICENSE](LICENSE).

## Architecture (MVP)

| Component | Role |
|-----------|------|
| `novbot-center` | MySQL-backed config authority; gRPC `Control.Connect` bidi stream; HTTP admin API |
| `novbot-node` | Long-running daemon; pull-on-boot config into memory; scheduled / dispatched probes; report + retry spool; single-file egress |
| `novbot-core` | Shared JSON specs, probes, schedules, retry spool |
| `novbot-proto` | `novbot.v1.Control` protobuf |

**Out of OSS MVP:** EE reports, UI, design docs in-repo.

## Prerequisites

- Rust stable (1.85+)
- `protoc` (protobuf compiler)
- **MySQL 8** (bring-your-own is primary). Optional: `docker compose up -d` for a local instance.

## Quick start

### 1. MySQL

```bash
# Optional local MySQL
docker compose up -d

export NOVBOT_DATABASE_URL='mysql://novbot:novbot@127.0.0.1:3306/novbot'
```

Center applies `crates/novbot-center/migrations/001_init.sql` on startup.

### 2. Build

```bash
cargo build --workspace
cargo test --workspace
```

### 3. Run center

```bash
export NOVBOT_DATABASE_URL='mysql://novbot:novbot@127.0.0.1:3306/novbot'
# optional: export NOVBOT_BOOTSTRAP_TOKEN=secret
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
      {"id":"root-exists","kind":"compliance_path","params":{"path":"/","must_exist":true}}
    ],
    "schedules": [
      {"spec_id":"cpu","interval_secs":30,"enabled":true},
      {"spec_id":"mem","interval_secs":60,"enabled":true}
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

### HTTP API

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Liveness |
| GET | `/v1/nodes` | List registered nodes |
| GET/PUT | `/v1/nodes/:id/config` | Get/put specs (+ optional schedules) |
| GET/PUT | `/v1/nodes/:id/schedules` | Get/put schedules |
| GET | `/v1/results?node_id=&limit=` | Recent probe results |

### gRPC

`Control.Connect` bi-directional stream: Register, Heartbeat, PullConfig, ReportResult, Ack; server may PushConfig / Dispatch / PushSchedule.

## Repositories

- Public OSS: `trendoverflow/novbot`
- Commercial enterprise artifacts: `trendoverflow/novbot-enterprise` (proprietary)
