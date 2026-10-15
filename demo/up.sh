#!/usr/bin/env bash
# Bring up demo MySQL (mysql:8.4) using demo/.env — DEMO ONLY.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEMO="$ROOT/demo"
ENV_FILE="$DEMO/.env"

if [[ ! -f "$ENV_FILE" ]]; then
  echo "Missing $ENV_FILE"
  echo "  cp demo/.env.example demo/.env"
  echo "Edit DEMO-ONLY secrets before continuing."
  exit 1
fi

set -a
# shellcheck disable=SC1090
source "$ENV_FILE"
set +a

: "${NOVBOT_DATABASE_URL:?NOVBOT_DATABASE_URL must be set in demo/.env}"
: "${MYSQL_ROOT_PASSWORD:?MYSQL_ROOT_PASSWORD must be set in demo/.env}"

echo "==> Starting MySQL (mysql:8.4) [demo compose overlay]"
cd "$ROOT"
docker compose --env-file "$ENV_FILE" \
  -f docker-compose.yml -f demo/docker-compose.yml \
  up -d

echo "==> Waiting for MySQL healthy..."
for i in $(seq 1 60); do
  if docker compose --env-file "$ENV_FILE" -f docker-compose.yml -f demo/docker-compose.yml \
    exec -T mysql mysqladmin ping -h 127.0.0.1 -uroot -p"${MYSQL_ROOT_PASSWORD}" --silent 2>/dev/null; then
    echo "MySQL is up."
    break
  fi
  if [[ "$i" -eq 60 ]]; then
    echo "MySQL did not become ready in time" >&2
    exit 1
  fi
  sleep 2
done

HTTP_ADDR="${NOVBOT_HTTP_ADDR:-0.0.0.0:8080}"
GRPC_ADDR="${NOVBOT_GRPC_ADDR:-0.0.0.0:50051}"
GRPC_PORT="${NOVBOT_GRPC_PORT:-50051}"

cat <<MSG

Demo MySQL is running.

Next — center (separate terminal; keep NOVBOT_API_TOKEN set for demo auth):
  set -a; source demo/.env; set +a
  cargo run -p novbot-center -- \\
    --database-url "\$NOVBOT_DATABASE_URL" \\
    --grpc-addr ${GRPC_ADDR} \\
    --http-addr ${HTTP_ADDR}

Then — node:
  set -a; source demo/.env; set +a
  cargo run -p novbot-node -- \\
    --center-grpc "http://127.0.0.1:${GRPC_PORT}" \\
    --node-id demo-1 \\
    --data-dir ./data/demo-1

Console (optional):
  cd console && npm ci && npm run dev
  # Settings: paste NOVBOT_API_TOKEN; leave base URL empty for Vite /v1 proxy

Quality / smoke:
  cargo test --workspace
  (cd console && npm run build)
  ./demo/smoke.sh

Record commit SHA after a green smoke:
  git rev-parse HEAD
MSG
