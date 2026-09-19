#!/usr/bin/env bash
# Tear down demo compose stack. Pass --volumes to drop MySQL data.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEMO="$ROOT/demo"
if [[ -f "$DEMO/.env" ]]; then
  ENV_FILE="$DEMO/.env"
else
  ENV_FILE="$DEMO/.env.example"
fi

cd "$ROOT"
if [[ "${1:-}" == "--volumes" || "${1:-}" == "-v" ]]; then
  docker compose --env-file "$ENV_FILE" -f docker-compose.yml -f demo/docker-compose.yml down -v
else
  docker compose --env-file "$ENV_FILE" -f docker-compose.yml -f demo/docker-compose.yml down
  echo "Tip: pass --volumes to also remove the MySQL data volume."
fi
