#!/usr/bin/env bash
#
# test-hls-session.sh — End-to-end HLS Content Steering session tests
#
# Tests the full HLS client lifecycle against a running apex-edge-steering server:
#   1. Initial state encoding (manifest updater)
#   2. First steering request (no pathway yet)
#   3. Follow-up with pathway + throughput
#   4. State accumulation across requests
#   5. Token passthrough across RELOAD-URIs
#
# Usage: ./scripts/test-hls-session.sh [base_url]

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
  # Extract a JSON field value. Usage: extract_json '{"a":1}' '.a'
  echo "$1" | python3 -c "import sys,json; d=json.load(sys.stdin); print(json.dumps(d$(echo $2)))" 2>/dev/null
}

extract_reload_query() {
  # Extract query string from RELOAD-URI
  local uri
  uri=$(echo "$1" | python3 -c "import sys,json; print(json.load(sys.stdin).get('RELOAD-URI',''))")
  echo "${uri#*\?}"
}

# ─── Reset server state ──────────────────────────────────────────────────────

curl -s -X POST "$BASE/reset" > /dev/null

echo ""
bold "═══ HLS Content Steering Session Tests ═══"
echo ""
echo "Server: $BASE"
echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 1: Encode initial state (manifest updater flow)
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 1: Encode initial session state ──"
echo ""

ENCODE_RESP=$(curl -s -X POST "$BASE/encode-state" \
  -H "Content-Type: application/json" \
  -d '{
    "priorities": ["cdn-a", "cdn-b"],
    "throughput_map": [],
    "min_bitrate": 783322,
    "max_bitrate": 4530860,
    "duration": 3600,
    "position": 0,
    "timestamp": 1700000000,
    "override_gen": 0
  }')

ENCODED_STATE=$(echo "$ENCODE_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['encoded'])")
SERVER_URI=$(echo "$ENCODE_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['server_uri'])")

assert_contains "encoded state is non-empty" "$ENCODED_STATE" "eyJ"
assert_contains "server_uri contains _ss param" "$SERVER_URI" "_ss="

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 2: First HLS steering request (using encoded state, no pathway yet)
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 2: First HLS request (initial state, no pathway) ──"
echo ""

RESP1=$(curl -s "$BASE/steer/hls?session=abc123&_ss=$ENCODED_STATE")

VERSION=$(extract_json "$RESP1" '["VERSION"]')
TTL=$(extract_json "$RESP1" '["TTL"]')
PATHWAY_PRI=$(extract_json "$RESP1" '["PATHWAY-PRIORITY"]')
RELOAD_URI_RAW=$(echo "$RESP1" | python3 -c "import sys,json; print(json.load(sys.stdin).get('RELOAD-URI',''))")

assert_eq "VERSION is 1" "1" "$VERSION"
assert_eq "TTL is 300 (default)" "300" "$TTL"
assert_eq "PATHWAY-PRIORITY is [cdn-a, cdn-b]" '["cdn-a", "cdn-b"]' "$PATHWAY_PRI"
assert_contains "RELOAD-URI contains _ss" "$RELOAD_URI_RAW" "_ss="
assert_contains "RELOAD-URI preserves session token" "$RELOAD_URI_RAW" "session=abc123"
assert_not_contains "no SERVICE-LOCATION-PRIORITY in HLS" "$RESP1" "SERVICE-LOCATION-PRIORITY"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 3: Follow-up HLS request with pathway and good throughput
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 3: HLS request with pathway + good throughput ──"
echo ""

QUERY2=$(extract_reload_query "$RESP1")
RESP2=$(curl -s "$BASE/steer/hls?${QUERY2}&_HLS_pathway=cdn-a&_HLS_throughput=5140000")

VERSION2=$(extract_json "$RESP2" '["VERSION"]')
TTL2=$(extract_json "$RESP2" '["TTL"]')
PATHWAY_PRI2=$(extract_json "$RESP2" '["PATHWAY-PRIORITY"]')
RELOAD_URI2=$(echo "$RESP2" | python3 -c "import sys,json; print(json.load(sys.stdin).get('RELOAD-URI',''))")

assert_eq "VERSION is 1" "1" "$VERSION2"
assert_eq "TTL is 300 (throughput healthy)" "300" "$TTL2"
assert_eq "priorities unchanged (healthy)" '["cdn-a", "cdn-b"]' "$PATHWAY_PRI2"
assert_contains "session token persists in RELOAD-URI" "$RELOAD_URI2" "session=abc123"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 4: Third request — verify state accumulation
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 4: State accumulation across requests ──"
echo ""

QUERY3=$(extract_reload_query "$RESP2")
RESP3=$(curl -s "$BASE/steer/hls?${QUERY3}&_HLS_pathway=cdn-a&_HLS_throughput=6000000")

TTL3=$(extract_json "$RESP3" '["TTL"]')
PATHWAY_PRI3=$(extract_json "$RESP3" '["PATHWAY-PRIORITY"]')

assert_eq "TTL still 300 (healthy)" "300" "$TTL3"
assert_eq "priorities stable" '["cdn-a", "cdn-b"]' "$PATHWAY_PRI3"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 5: HLS request with Akamai EdgeAuth tokens (passthrough)
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 5: Akamai token passthrough ──"
echo ""

RESP_TOKEN=$(curl -s "$BASE/steer/hls?start=1772770805&end=1772857805&userId=93334984&hashParam=a7614ed1&_ss=$ENCODED_STATE&_HLS_pathway=cdn-a&_HLS_throughput=5000000")

TOKEN_URI=$(echo "$RESP_TOKEN" | python3 -c "import sys,json; print(json.load(sys.stdin).get('RELOAD-URI',''))")

assert_contains "start token preserved" "$TOKEN_URI" "start=1772770805"
assert_contains "end token preserved" "$TOKEN_URI" "end=1772857805"
assert_contains "userId token preserved" "$TOKEN_URI" "userId=93334984"
assert_contains "hashParam token preserved" "$TOKEN_URI" "hashParam=a7614ed1"

# Follow-up to verify tokens persist across hops
QUERY_T2=$(extract_reload_query "$RESP_TOKEN")
RESP_TOKEN2=$(curl -s "$BASE/steer/hls?${QUERY_T2}&_HLS_pathway=cdn-a&_HLS_throughput=6000000")
TOKEN_URI2=$(echo "$RESP_TOKEN2" | python3 -c "import sys,json; print(json.load(sys.stdin).get('RELOAD-URI',''))")

assert_contains "start token persists hop 2" "$TOKEN_URI2" "start=1772770805"
assert_contains "hashParam persists hop 2" "$TOKEN_URI2" "hashParam=a7614ed1"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 6: Auto-detect HLS from _HLS_ params (no path hint)
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 6: Protocol auto-detection from _HLS_ params ──"
echo ""

RESP_AUTO=$(curl -s "$BASE/steer?_ss=$ENCODED_STATE&_HLS_pathway=cdn-a&_HLS_throughput=5000000")

assert_contains "auto-detected HLS: PATHWAY-PRIORITY present" "$RESP_AUTO" "PATHWAY-PRIORITY"
assert_not_contains "auto-detected HLS: no SERVICE-LOCATION-PRIORITY" "$RESP_AUTO" "SERVICE-LOCATION-PRIORITY"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 7: JSON response format matches HLS Content Steering spec
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 7: HLS response JSON format validation ──"
echo ""

# Validate all required fields per HLS Content Steering spec v1.2b1
HAS_VERSION=$(echo "$RESP1" | python3 -c "import sys,json; d=json.load(sys.stdin); print('yes' if 'VERSION' in d else 'no')")
HAS_TTL=$(echo "$RESP1" | python3 -c "import sys,json; d=json.load(sys.stdin); print('yes' if 'TTL' in d else 'no')")
HAS_RELOAD=$(echo "$RESP1" | python3 -c "import sys,json; d=json.load(sys.stdin); print('yes' if 'RELOAD-URI' in d else 'no')")
HAS_PATHWAY=$(echo "$RESP1" | python3 -c "import sys,json; d=json.load(sys.stdin); print('yes' if 'PATHWAY-PRIORITY' in d else 'no')")
HAS_SLP=$(echo "$RESP1" | python3 -c "import sys,json; d=json.load(sys.stdin); print('yes' if 'SERVICE-LOCATION-PRIORITY' in d else 'no')")

assert_eq "has VERSION field" "yes" "$HAS_VERSION"
assert_eq "has TTL field" "yes" "$HAS_TTL"
assert_eq "has RELOAD-URI field" "yes" "$HAS_RELOAD"
assert_eq "has PATHWAY-PRIORITY field" "yes" "$HAS_PATHWAY"
assert_eq "no SERVICE-LOCATION-PRIORITY in HLS response" "no" "$HAS_SLP"

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
