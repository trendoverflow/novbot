#!/usr/bin/env bash
# Seed OrbStack demo node config (center-authoritative). Safe to re-run.
# Default: Mac tunnel http://127.0.0.1:18080 + node orb-arm-1 + demo Bearer.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BASE="${NOVBOT_SEED_BASE:-http://127.0.0.1:18080}"
TOKEN="${NOVBOT_API_TOKEN:-demo-api-token-change-me}"
NODE_ID="${NOVBOT_SEED_NODE_ID:-orb-arm-1}"
BODY="${NOVBOT_SEED_FILE:-$ROOT/demo/seed-orb-arm-1.json}"

if [[ ! -f "$BODY" ]]; then
  echo "missing seed file: $BODY" >&2
  exit 1
fi

echo "==> PUT $BASE/v1/nodes/${NODE_ID}/config"
code=$(curl -sS -o /tmp/novbot-seed-out.json -w "%{http_code}" \
  -X PUT \
  -H "Authorization: Bearer ${TOKEN}" \
  -H "content-type: application/json" \
  --data-binary @"$BODY" \
  "$BASE/v1/nodes/${NODE_ID}/config" || true)
echo "HTTP $code"
cat /tmp/novbot-seed-out.json
echo
[[ "$code" == "200" ]] || exit 1
echo "OK seeded ${NODE_ID} (use kind:cpu probe — not skill id cpu)"
