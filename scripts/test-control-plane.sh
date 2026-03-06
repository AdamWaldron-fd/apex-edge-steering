#!/usr/bin/env bash
#
# test-control-plane.sh — End-to-end master control plane + QoE tests
#
# Tests master server interactions and QoE-triggered CDN switching:
#   1. set_priorities override
#   2. exclude_pathway (disaster recovery)
#   3. clear_overrides
#   4. Generation-based idempotency (stale command rejection)
#   5. Override affects subsequent steering requests
#   6. QoE-triggered CDN demotion (degraded throughput)
#   7. QoE recovery (TTL returns to normal)
#   8. Combined: master override + QoE interaction
#
# Usage: ./scripts/test-control-plane.sh [base_url]

set -euo pipefail

BASE="${1:-http://localhost:3001}"
PASS=0
FAIL=0
TESTS=()

# ─── Helpers ──────────────────────────────────────────────────────────────────

red()   { printf "\033[31m%s\033[0m" "$1"; }
green() { printf "\033[32m%s\033[0m" "$1"; }
bold()  { printf "\033[1m%s\033[0m" "$1"; }

assert_eq() {
  local desc="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    PASS=$((PASS + 1))
    TESTS+=("$(green "PASS") $desc")
  else
    FAIL=$((FAIL + 1))
    TESTS+=("$(red "FAIL") $desc\n       expected: $expected\n       actual:   $actual")
  fi
}

assert_contains() {
  local desc="$1" haystack="$2" needle="$3"
  if echo "$haystack" | grep -q "$needle"; then
    PASS=$((PASS + 1))
    TESTS+=("$(green "PASS") $desc")
  else
    FAIL=$((FAIL + 1))
    TESTS+=("$(red "FAIL") $desc\n       expected to contain: $needle\n       actual: $haystack")
  fi
}

assert_not_contains() {
  local desc="$1" haystack="$2" needle="$3"
  if echo "$haystack" | grep -q "$needle"; then
    FAIL=$((FAIL + 1))
    TESTS+=("$(red "FAIL") $desc\n       expected NOT to contain: $needle\n       actual: $haystack")
  else
    PASS=$((PASS + 1))
    TESTS+=("$(green "PASS") $desc")
  fi
}

extract_json() {
  echo "$1" | python3 -c "import sys,json; d=json.load(sys.stdin); print(json.dumps(d$(echo $2)))" 2>/dev/null
}

extract_reload_query() {
  local uri
  uri=$(echo "$1" | python3 -c "import sys,json; print(json.load(sys.stdin).get('RELOAD-URI',''))")
  echo "${uri#*\?}"
}

# ─── Reset server state ──────────────────────────────────────────────────────

curl -s -X POST "$BASE/reset" > /dev/null

echo ""
bold "═══ Control Plane & QoE Tests ═══"
echo ""
echo "Server: $BASE"
echo ""

# ─── Encode initial state ────────────────────────────────────────────────────

ENCODE_RESP=$(curl -s -X POST "$BASE/encode-state" \
  -H "Content-Type: application/json" \
  -d '{
    "priorities": ["cdn-a", "cdn-b", "cdn-c"],
    "throughput_map": [],
    "min_bitrate": 1000000,
    "max_bitrate": 8000000,
    "duration": 3600,
    "position": 0,
    "timestamp": 1700000000,
    "override_gen": 0
  }')

SS=$(echo "$ENCODE_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['encoded'])")

# ═══════════════════════════════════════════════════════════════════════════════
# Test 1: set_priorities command
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 1: set_priorities command ──"
echo ""

CTRL1=$(curl -s -X POST "$BASE/control" \
  -H "Content-Type: application/json" \
  -d '{
    "type": "set_priorities",
    "region": null,
    "priorities": ["cdn-c", "cdn-a"],
    "generation": 1,
    "ttl_override": 30
  }')

GEN=$(extract_json "$CTRL1" '["generation"]')
PRI=$(extract_json "$CTRL1" '["priority_override"]["priorities"]')
TTL_OV=$(extract_json "$CTRL1" '["priority_override"]["ttl_override"]')

assert_eq "generation is 1" "1" "$GEN"
assert_eq "priorities set to [cdn-c, cdn-a]" '["cdn-c", "cdn-a"]' "$PRI"
assert_eq "ttl_override is 30" "30" "$TTL_OV"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 2: Override affects steering responses
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 2: Override affects steering response ──"
echo ""

RESP=$(curl -s "$BASE/steer/hls?_ss=$SS&_HLS_pathway=cdn-a&_HLS_throughput=5000000")

PATHWAY_PRI=$(extract_json "$RESP" '["PATHWAY-PRIORITY"]')
TTL=$(extract_json "$RESP" '["TTL"]')

assert_eq "priorities overridden to [cdn-c, cdn-a]" '["cdn-c", "cdn-a"]' "$PATHWAY_PRI"
assert_eq "TTL overridden to 30" "30" "$TTL"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 3: Stale command rejected (generation <= current)
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 3: Stale command rejection (idempotency) ──"
echo ""

CTRL_STALE=$(curl -s -X POST "$BASE/control" \
  -H "Content-Type: application/json" \
  -d '{
    "type": "set_priorities",
    "region": null,
    "priorities": ["cdn-b"],
    "generation": 1,
    "ttl_override": null
  }')

# Generation should still be 1 (stale command rejected)
GEN_STALE=$(extract_json "$CTRL_STALE" '["generation"]')
PRI_STALE=$(extract_json "$CTRL_STALE" '["priority_override"]["priorities"]')

assert_eq "generation unchanged (stale rejected)" "1" "$GEN_STALE"
assert_eq "priorities unchanged (stale rejected)" '["cdn-c", "cdn-a"]' "$PRI_STALE"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 4: exclude_pathway command
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 4: exclude_pathway command ──"
echo ""

CTRL_EXCL=$(curl -s -X POST "$BASE/control" \
  -H "Content-Type: application/json" \
  -d '{
    "type": "exclude_pathway",
    "region": null,
    "pathway": "cdn-c",
    "generation": 2
  }')

GEN_EXCL=$(extract_json "$CTRL_EXCL" '["generation"]')
EXCLUDED=$(extract_json "$CTRL_EXCL" '["excluded_pathways"]')

assert_eq "generation advanced to 2" "2" "$GEN_EXCL"
assert_eq "cdn-c excluded" '["cdn-c"]' "$EXCLUDED"

# Steering response should not include cdn-c
RESP_EXCL=$(curl -s "$BASE/steer/hls?_ss=$SS&_HLS_pathway=cdn-a&_HLS_throughput=5000000")
PATHWAY_EXCL=$(extract_json "$RESP_EXCL" '["PATHWAY-PRIORITY"]')

assert_eq "cdn-c removed from response" '["cdn-a"]' "$PATHWAY_EXCL"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 5: clear_overrides command
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 5: clear_overrides command ──"
echo ""

CTRL_CLEAR=$(curl -s -X POST "$BASE/control" \
  -H "Content-Type: application/json" \
  -d '{
    "type": "clear_overrides",
    "region": null,
    "generation": 3
  }')

GEN_CLEAR=$(extract_json "$CTRL_CLEAR" '["generation"]')
HAS_OVERRIDE=$(echo "$CTRL_CLEAR" | python3 -c "import sys,json; d=json.load(sys.stdin); print('null' if d['priority_override'] is None else 'set')")
EXCLUDED_CLEAR=$(extract_json "$CTRL_CLEAR" '["excluded_pathways"]')

assert_eq "generation advanced to 3" "3" "$GEN_CLEAR"
assert_eq "priority_override cleared" "null" "$HAS_OVERRIDE"
assert_eq "excluded_pathways cleared" "[]" "$EXCLUDED_CLEAR"

# Steering should return original priorities
RESP_CLEAR=$(curl -s "$BASE/steer/hls?_ss=$SS&_HLS_pathway=cdn-a&_HLS_throughput=5000000")
PATHWAY_CLEAR=$(extract_json "$RESP_CLEAR" '["PATHWAY-PRIORITY"]')
TTL_CLEAR=$(extract_json "$RESP_CLEAR" '["TTL"]')

assert_eq "priorities restored to original" '["cdn-a", "cdn-b", "cdn-c"]' "$PATHWAY_CLEAR"
assert_eq "TTL back to default 300" "300" "$TTL_CLEAR"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 6: Bad command returns 400
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 6: Malformed command returns 400 ──"
echo ""

HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$BASE/control" \
  -H "Content-Type: application/json" \
  -d '{"type": "invalid_command"}')

assert_eq "invalid command returns 400" "400" "$HTTP_CODE"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 7: QoE — degraded throughput triggers CDN demotion
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 7: QoE — degraded throughput demotes CDN ──"
echo ""

# Reset to clean state
curl -s -X POST "$BASE/reset" > /dev/null

# Request with degraded throughput: 500K < 1.2 * 1M = 1.2M threshold
RESP_QOE=$(curl -s "$BASE/steer/hls?_ss=$SS&_HLS_pathway=cdn-a&_HLS_throughput=500000")

PATHWAY_QOE=$(extract_json "$RESP_QOE" '["PATHWAY-PRIORITY"]')
TTL_QOE=$(extract_json "$RESP_QOE" '["TTL"]')

# cdn-a should be demoted, cdn-b promoted to top
QOE_TOP=$(echo "$RESP_QOE" | python3 -c "import sys,json; print(json.load(sys.stdin)['PATHWAY-PRIORITY'][0])")

assert_eq "degraded cdn-a demoted from top" "cdn-b" "$QOE_TOP"
assert_eq "TTL reduced to 10 (QoE fast poll)" "10" "$TTL_QOE"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 8: QoE — healthy throughput keeps normal TTL
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 8: QoE — healthy throughput keeps normal TTL ──"
echo ""

RESP_HEALTHY=$(curl -s "$BASE/steer/hls?_ss=$SS&_HLS_pathway=cdn-a&_HLS_throughput=5000000")

TTL_HEALTHY=$(extract_json "$RESP_HEALTHY" '["TTL"]')
PRI_HEALTHY=$(echo "$RESP_HEALTHY" | python3 -c "import sys,json; print(json.load(sys.stdin)['PATHWAY-PRIORITY'][0])")

assert_eq "healthy throughput keeps cdn-a on top" "cdn-a" "$PRI_HEALTHY"
assert_eq "TTL stays at 300" "300" "$TTL_HEALTHY"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 9: QoE — exactly at threshold is not degraded
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 9: QoE — exactly at threshold (edge case) ──"
echo ""

# Threshold = 1.2 * 1,000,000 = 1,200,000. Exactly at threshold is NOT degraded.
RESP_EDGE=$(curl -s "$BASE/steer/hls?_ss=$SS&_HLS_pathway=cdn-a&_HLS_throughput=1200000")

TTL_EDGE=$(extract_json "$RESP_EDGE" '["TTL"]')
PRI_EDGE=$(echo "$RESP_EDGE" | python3 -c "import sys,json; print(json.load(sys.stdin)['PATHWAY-PRIORITY'][0])")

assert_eq "at threshold: cdn-a stays on top" "cdn-a" "$PRI_EDGE"
assert_eq "at threshold: TTL is 300" "300" "$TTL_EDGE"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 10: QoE — just below threshold triggers demotion
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 10: QoE — just below threshold ──"
echo ""

RESP_BELOW=$(curl -s "$BASE/steer/hls?_ss=$SS&_HLS_pathway=cdn-a&_HLS_throughput=1199999")

TTL_BELOW=$(extract_json "$RESP_BELOW" '["TTL"]')
PRI_BELOW=$(echo "$RESP_BELOW" | python3 -c "import sys,json; print(json.load(sys.stdin)['PATHWAY-PRIORITY'][0])")

assert_eq "below threshold: cdn-a demoted" "cdn-b" "$PRI_BELOW"
assert_eq "below threshold: TTL is 10" "10" "$TTL_BELOW"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 11: QoE recovery — full cycle (good → degraded → recovered)
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 11: QoE full cycle (good → degraded → recovered) ──"
echo ""

# Step 1: Good throughput
RESP_C1=$(curl -s "$BASE/steer/hls?_ss=$SS&_HLS_pathway=cdn-a&_HLS_throughput=5000000")
TTL_C1=$(extract_json "$RESP_C1" '["TTL"]')
PRI_C1=$(echo "$RESP_C1" | python3 -c "import sys,json; print(json.load(sys.stdin)['PATHWAY-PRIORITY'][0])")

assert_eq "cycle step 1: cdn-a on top" "cdn-a" "$PRI_C1"
assert_eq "cycle step 1: TTL=300" "300" "$TTL_C1"

# Step 2: Degraded — use RELOAD-URI from step 1
Q2=$(extract_reload_query "$RESP_C1")
RESP_C2=$(curl -s "$BASE/steer/hls?${Q2}&_HLS_pathway=cdn-a&_HLS_throughput=500000")
TTL_C2=$(extract_json "$RESP_C2" '["TTL"]')
PRI_C2=$(echo "$RESP_C2" | python3 -c "import sys,json; print(json.load(sys.stdin)['PATHWAY-PRIORITY'][0])")

assert_eq "cycle step 2: cdn-a demoted" "cdn-b" "$PRI_C2"
assert_eq "cycle step 2: TTL=10" "10" "$TTL_C2"

# Step 3: Recovered on cdn-b — use RELOAD-URI from step 2
Q3=$(extract_reload_query "$RESP_C2")
RESP_C3=$(curl -s "$BASE/steer/hls?${Q3}&_HLS_pathway=cdn-b&_HLS_throughput=6000000")
TTL_C3=$(extract_json "$RESP_C3" '["TTL"]')
PRI_C3=$(echo "$RESP_C3" | python3 -c "import sys,json; print(json.load(sys.stdin)['PATHWAY-PRIORITY'][0])")

assert_eq "cycle step 3: cdn-b stays on top" "cdn-b" "$PRI_C3"
assert_eq "cycle step 3: TTL=300 (recovered)" "300" "$TTL_C3"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 12: Master override during active QoE demotion
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 12: Master override + QoE interaction ──"
echo ""

# Push master override
curl -s -X POST "$BASE/control" \
  -H "Content-Type: application/json" \
  -d '{
    "type": "set_priorities",
    "region": null,
    "priorities": ["cdn-c", "cdn-b", "cdn-a"],
    "generation": 10,
    "ttl_override": 60
  }' > /dev/null

# Request — master override should take effect
RESP_COMBO=$(curl -s "$BASE/steer/hls?_ss=$SS&_HLS_pathway=cdn-c&_HLS_throughput=5000000")
PRI_COMBO=$(extract_json "$RESP_COMBO" '["PATHWAY-PRIORITY"]')
TTL_COMBO=$(extract_json "$RESP_COMBO" '["TTL"]')

assert_eq "master override applied" '["cdn-c", "cdn-b", "cdn-a"]' "$PRI_COMBO"
assert_eq "master TTL override (60)" "60" "$TTL_COMBO"

# Now with degraded throughput on cdn-c — QoE should still kick in
RESP_COMBO2=$(curl -s "$BASE/steer/hls?_ss=$SS&_HLS_pathway=cdn-c&_HLS_throughput=500000")
PRI_COMBO2=$(echo "$RESP_COMBO2" | python3 -c "import sys,json; print(json.load(sys.stdin)['PATHWAY-PRIORITY'][0])")
TTL_COMBO2=$(extract_json "$RESP_COMBO2" '["TTL"]')

assert_eq "QoE demotes cdn-c even with master override" "cdn-b" "$PRI_COMBO2"
assert_eq "QoE TTL (10) overrides master TTL (60)" "10" "$TTL_COMBO2"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 13: Disaster recovery sequence (exclude → clear)
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 13: Disaster recovery sequence ──"
echo ""

# Reset
curl -s -X POST "$BASE/reset" > /dev/null

# Exclude cdn-a
curl -s -X POST "$BASE/control" \
  -H "Content-Type: application/json" \
  -d '{"type":"exclude_pathway","region":null,"pathway":"cdn-a","generation":1}' > /dev/null

RESP_DR1=$(curl -s "$BASE/steer/hls?_ss=$SS&_HLS_pathway=cdn-a&_HLS_throughput=0")
PRI_DR1=$(extract_json "$RESP_DR1" '["PATHWAY-PRIORITY"]')

assert_not_contains "cdn-a excluded from response" "$PRI_DR1" "cdn-a"
assert_contains "cdn-b in response" "$PRI_DR1" "cdn-b"

# Clear overrides (cdn-a recovered)
curl -s -X POST "$BASE/control" \
  -H "Content-Type: application/json" \
  -d '{"type":"clear_overrides","region":null,"generation":2}' > /dev/null

RESP_DR2=$(curl -s "$BASE/steer/hls?_ss=$SS&_HLS_pathway=cdn-a&_HLS_throughput=5000000")
PRI_DR2=$(extract_json "$RESP_DR2" '["PATHWAY-PRIORITY"]')

assert_contains "cdn-a restored after clear" "$PRI_DR2" "cdn-a"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 14: Health check includes override state
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 14: Health endpoint shows override status ──"
echo ""

HEALTH=$(curl -s "$BASE/health")
assert_contains "health check OK" "$HEALTH" '"status":"ok"'
assert_contains "health check has engine" "$HEALTH" '"engine":"apex-edge-steering"'

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 15: Master override takes precedence over client state across hops
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 15: Master override takes precedence over client state ──"
echo ""

# Reset
curl -s -X POST "$BASE/reset" > /dev/null

# Request 1: No override — use client state priorities
RESP_S1=$(curl -s "$BASE/steer/hls?_ss=$SS&_HLS_pathway=cdn-a&_HLS_throughput=5000000")
PRI_S1=$(echo "$RESP_S1" | python3 -c "import sys,json; print(json.load(sys.stdin)['PATHWAY-PRIORITY'][0])")
assert_eq "hop 1: client state priorities (cdn-a)" "cdn-a" "$PRI_S1"

# Master pushes override
curl -s -X POST "$BASE/control" \
  -H "Content-Type: application/json" \
  -d '{
    "type": "set_priorities",
    "region": null,
    "priorities": ["cdn-c", "cdn-b"],
    "generation": 1,
    "ttl_override": 20
  }' > /dev/null

# Request 2: Client sends RELOAD-URI from request 1 (old state), override must apply
Q_S2=$(extract_reload_query "$RESP_S1")
RESP_S2=$(curl -s "$BASE/steer/hls?${Q_S2}&_HLS_pathway=cdn-a&_HLS_throughput=5000000")
PRI_S2=$(extract_json "$RESP_S2" '["PATHWAY-PRIORITY"]')
TTL_S2=$(extract_json "$RESP_S2" '["TTL"]')

assert_eq "hop 2: master override applied [cdn-c, cdn-b]" '["cdn-c", "cdn-b"]' "$PRI_S2"
assert_eq "hop 2: master TTL override (20)" "20" "$TTL_S2"

# Request 3: Client sends RELOAD-URI from request 2 — override STILL applied
Q_S3=$(extract_reload_query "$RESP_S2")
RESP_S3=$(curl -s "$BASE/steer/hls?${Q_S3}&_HLS_pathway=cdn-c&_HLS_throughput=6000000")
PRI_S3=$(extract_json "$RESP_S3" '["PATHWAY-PRIORITY"]')
TTL_S3=$(extract_json "$RESP_S3" '["TTL"]')

assert_eq "hop 3: master override persists [cdn-c, cdn-b]" '["cdn-c", "cdn-b"]' "$PRI_S3"
assert_eq "hop 3: master TTL override still active (20)" "20" "$TTL_S3"

# Master pushes NEW override with different priorities
curl -s -X POST "$BASE/control" \
  -H "Content-Type: application/json" \
  -d '{
    "type": "set_priorities",
    "region": null,
    "priorities": ["cdn-b", "cdn-a"],
    "generation": 2,
    "ttl_override": 15
  }' > /dev/null

# Request 4: Client sends RELOAD-URI from request 3 — NEW override applied
Q_S4=$(extract_reload_query "$RESP_S3")
RESP_S4=$(curl -s "$BASE/steer/hls?${Q_S4}&_HLS_pathway=cdn-c&_HLS_throughput=6000000")
PRI_S4=$(extract_json "$RESP_S4" '["PATHWAY-PRIORITY"]')
TTL_S4=$(extract_json "$RESP_S4" '["TTL"]')

assert_eq "hop 4: new master override applied [cdn-b, cdn-a]" '["cdn-b", "cdn-a"]' "$PRI_S4"
assert_eq "hop 4: new master TTL override (15)" "15" "$TTL_S4"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 16: Master override with DASH protocol across hops
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 16: Master override with DASH across hops ──"
echo ""

# Reset
curl -s -X POST "$BASE/reset" > /dev/null

# Master pushes override BEFORE first request
curl -s -X POST "$BASE/control" \
  -H "Content-Type: application/json" \
  -d '{
    "type": "set_priorities",
    "region": null,
    "priorities": ["cdn-c", "cdn-a"],
    "generation": 1,
    "ttl_override": 25
  }' > /dev/null

# DASH request with client state that has different priorities
RESP_D1=$(curl -s "$BASE/steer/dash?_ss=$SS&_DASH_pathway=cdn-a&_DASH_throughput=5000000")
PRI_D1=$(extract_json "$RESP_D1" '["SERVICE-LOCATION-PRIORITY"]')
TTL_D1=$(extract_json "$RESP_D1" '["TTL"]')

assert_eq "DASH hop 1: master override applied" '["cdn-c", "cdn-a"]' "$PRI_D1"
assert_eq "DASH hop 1: master TTL (25)" "25" "$TTL_D1"

# Follow-up DASH request using RELOAD-URI — still overridden
Q_D2=$(extract_reload_query "$RESP_D1")
RESP_D2=$(curl -s "$BASE/steer/dash?${Q_D2}&_DASH_pathway=cdn-c&_DASH_throughput=6000000")
PRI_D2=$(extract_json "$RESP_D2" '["SERVICE-LOCATION-PRIORITY"]')

assert_eq "DASH hop 2: master override persists" '["cdn-c", "cdn-a"]' "$PRI_D2"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Summary
# ═══════════════════════════════════════════════════════════════════════════════

echo ""
bold "─── Results ───"
echo ""
for t in "${TESTS[@]}"; do
  echo -e "  $t"
done
echo ""
echo "  $(green "$PASS passed"), $([ $FAIL -gt 0 ] && red "$FAIL failed" || echo "$FAIL failed")"
echo ""

exit $FAIL
