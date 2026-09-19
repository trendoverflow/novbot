#!/usr/bin/env bash
# Demo smoke: health, Bearer 401, authorized /v1, optional fleet push.
# Expects center already running (see demo/up.sh). Loads demo/.env when present.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEMO="$ROOT/demo"
ENV_FILE="$DEMO/.env"
[[ -f "$ENV_FILE" ]] || ENV_FILE="$DEMO/.env.example"

set -a
# shellcheck disable=SC1090
source "$ENV_FILE"
set +a

HTTP_PORT="${NOVBOT_HTTP_PORT:-8080}"
BASE="http://127.0.0.1:${HTTP_PORT}"
TOKEN="${NOVBOT_API_TOKEN:-}"
NODE_ID="${SMOKE_NODE_ID:-demo-1}"
FAIL=0

pass() { echo "PASS  $*"; }
fail() { echo "FAIL  $*" >&2; FAIL=1; }

echo "==> S1 quality (mentioned; run separately if slow)"
echo "    cargo test --workspace"
echo "    (cd console && npm run build)"
echo "    Tip: set SMOKE_RUN_QUALITY=1 to execute them from this script."

if [[ "${SMOKE_RUN_QUALITY:-}" == "1" ]]; then
  (cd "$ROOT" && cargo test --workspace) || fail "cargo test --workspace"
  (cd "$ROOT/console" && npm run build) || fail "console npm run build"
fi

echo "==> S2 GET /health (open)"
code=$(curl -sS -o /tmp/novbot-smoke-health.json -w "%{http_code}" "$BASE/health" || true)
if [[ "$code" == "200" ]]; then
  pass "GET /health -> 200"
else
  fail "GET /health -> $code (is center up on :${HTTP_PORT}?)"
fi

echo "==> S3 Bearer 401 when NOVBOT_API_TOKEN is set"
if [[ -z "$TOKEN" ]]; then
  echo "SKIP  NOVBOT_API_TOKEN empty — cannot assert 401 (auth optional today; demo should set it)"
else
  code=$(curl -sS -o /tmp/novbot-smoke-unauth.json -w "%{http_code}" "$BASE/v1/nodes" || true)
  if [[ "$code" == "401" ]]; then
    pass "GET /v1/nodes without Bearer -> 401"
  else
    fail "GET /v1/nodes without Bearer -> $code (expected 401)"
  fi

  code=$(curl -sS -o /tmp/novbot-smoke-auth.json -w "%{http_code}" \
    -H "Authorization: Bearer $TOKEN" "$BASE/v1/nodes" || true)
  if [[ "$code" == "200" ]]; then
    pass "GET /v1/nodes with Bearer -> 200"
  else
    fail "GET /v1/nodes with Bearer -> $code (expected 200)"
  fi
fi

echo "==> S4 optional fleet skill-group push (requires registered node)"
AUTH=()
[[ -n "$TOKEN" ]] && AUTH=(-H "Authorization: Bearer $TOKEN")
if [[ "${SMOKE_FLEET:-}" == "1" ]]; then
  code=$(curl -sS -o /tmp/novbot-smoke-fleet.json -w "%{http_code}" \
    "${AUTH[@]}" -H "content-type: application/json" \
    -X POST "$BASE/v1/fleet/skill-groups/push" \
    -d "{\"group_id\":\"host-basics\",\"node_ids\":[\"${NODE_ID}\"]}" || true)
  if [[ "$code" == "200" || "$code" == "202" ]]; then
    pass "POST /v1/fleet/skill-groups/push -> $code"
    cat /tmp/novbot-smoke-fleet.json
    echo
  else
    fail "POST /v1/fleet/skill-groups/push -> $code"
    cat /tmp/novbot-smoke-fleet.json 2>/dev/null || true
    echo
  fi
else
  echo "SKIP  set SMOKE_FLEET=1 to exercise fleet push against node ${NODE_ID}"
fi

if [[ "$FAIL" -ne 0 ]]; then
  echo "Smoke FAILED"
  exit 1
fi
echo "Smoke OK"
echo "Record SHA: $(cd "$ROOT" && git rev-parse HEAD)"
