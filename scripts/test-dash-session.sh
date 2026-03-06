#!/usr/bin/env bash
#
# test-dash-session.sh — End-to-end DASH Content Steering session tests
#
# Tests the full DASH client lifecycle against a running apex-edge-steering server:
#   1. queryBeforeStart request (no _DASH_ params)
#   2. Follow-up with pathway + throughput
#   3. Double-quoted pathway handling (DASH spec)
#   4. SERVICE-LOCATION-PRIORITY format validation
#   5. Token passthrough for DASH sessions
#
# Usage: ./scripts/test-dash-session.sh [base_url]

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
bold "═══ DASH Content Steering Session Tests ═══"
echo ""
echo "Server: $BASE"
echo ""

# ─── Encode initial DASH state ────────────────────────────────────────────────

ENCODE_RESP=$(curl -s -X POST "$BASE/encode-state" \
  -H "Content-Type: application/json" \
  -d '{
    "priorities": ["alpha", "beta", "gamma"],
    "throughput_map": [],
    "min_bitrate": 500000,
    "max_bitrate": 6000000,
    "duration": 7200,
    "position": 0,
    "timestamp": 1700000000,
    "override_gen": 0
  }')

ENCODED_STATE=$(echo "$ENCODE_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['encoded'])")

# ═══════════════════════════════════════════════════════════════════════════════
# Test 1: queryBeforeStart — first DASH request (no _DASH_ params)
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 1: queryBeforeStart (no _DASH_ params) ──"
echo ""

RESP1=$(curl -s "$BASE/steer/dash?token=234523452&_ss=$ENCODED_STATE")

VERSION=$(extract_json "$RESP1" '["VERSION"]')
TTL=$(extract_json "$RESP1" '["TTL"]')
SLP=$(extract_json "$RESP1" '["SERVICE-LOCATION-PRIORITY"]')
RELOAD1=$(echo "$RESP1" | python3 -c "import sys,json; print(json.load(sys.stdin).get('RELOAD-URI',''))")

assert_eq "VERSION is 1" "1" "$VERSION"
assert_eq "TTL is 300" "300" "$TTL"
assert_eq "SERVICE-LOCATION-PRIORITY has 3 entries" '["alpha", "beta", "gamma"]' "$SLP"
assert_not_contains "no PATHWAY-PRIORITY in DASH response" "$RESP1" "PATHWAY-PRIORITY"
assert_contains "RELOAD-URI preserves token" "$RELOAD1" "token=234523452"
assert_contains "RELOAD-URI has _ss" "$RELOAD1" "_ss="

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 2: Follow-up with unquoted _DASH_pathway and throughput
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 2: DASH request with pathway + throughput ──"
echo ""

QUERY2=$(extract_reload_query "$RESP1")
RESP2=$(curl -s "$BASE/steer/dash?${QUERY2}&_DASH_pathway=alpha&_DASH_throughput=5140000")

VERSION2=$(extract_json "$RESP2" '["VERSION"]')
TTL2=$(extract_json "$RESP2" '["TTL"]')
SLP2=$(extract_json "$RESP2" '["SERVICE-LOCATION-PRIORITY"]')
RELOAD2=$(echo "$RESP2" | python3 -c "import sys,json; print(json.load(sys.stdin).get('RELOAD-URI',''))")

assert_eq "VERSION is 1" "1" "$VERSION2"
assert_eq "TTL is 300 (throughput healthy)" "300" "$TTL2"
assert_eq "priorities unchanged" '["alpha", "beta", "gamma"]' "$SLP2"
assert_contains "token persists" "$RELOAD2" "token=234523452"
assert_not_contains "no PATHWAY-PRIORITY" "$RESP2" "PATHWAY-PRIORITY"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 3: Quoted _DASH_pathway (per DASH-IF spec, pathway values are double-quoted)
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 3: Double-quoted _DASH_pathway (spec compliance) ──"
echo ""

QUERY3=$(extract_reload_query "$RESP2")
# %22 = double-quote character, per DASH-IF Content Steering spec
RESP3=$(curl -s "$BASE/steer/dash?${QUERY3}&_DASH_pathway=%22alpha%22&_DASH_throughput=5140000")

SLP3=$(extract_json "$RESP3" '["SERVICE-LOCATION-PRIORITY"]')
TTL3=$(extract_json "$RESP3" '["TTL"]')

assert_eq "quoted pathway decoded correctly" '["alpha", "beta", "gamma"]' "$SLP3"
assert_eq "TTL normal" "300" "$TTL3"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 4: Auto-detect DASH from _DASH_ params (no path hint)
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 4: Protocol auto-detection from _DASH_ params ──"
echo ""

RESP_AUTO=$(curl -s "$BASE/steer?_ss=$ENCODED_STATE&_DASH_pathway=alpha&_DASH_throughput=5000000")

assert_contains "auto-detected DASH: SERVICE-LOCATION-PRIORITY present" "$RESP_AUTO" "SERVICE-LOCATION-PRIORITY"
assert_not_contains "auto-detected DASH: no PATHWAY-PRIORITY" "$RESP_AUTO" "PATHWAY-PRIORITY"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 5: DASH response JSON format validation
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 5: DASH response JSON format validation ──"
echo ""

HAS_VERSION=$(echo "$RESP1" | python3 -c "import sys,json; d=json.load(sys.stdin); print('yes' if 'VERSION' in d else 'no')")
HAS_TTL=$(echo "$RESP1" | python3 -c "import sys,json; d=json.load(sys.stdin); print('yes' if 'TTL' in d else 'no')")
HAS_RELOAD=$(echo "$RESP1" | python3 -c "import sys,json; d=json.load(sys.stdin); print('yes' if 'RELOAD-URI' in d else 'no')")
HAS_SLP=$(echo "$RESP1" | python3 -c "import sys,json; d=json.load(sys.stdin); print('yes' if 'SERVICE-LOCATION-PRIORITY' in d else 'no')")
HAS_PP=$(echo "$RESP1" | python3 -c "import sys,json; d=json.load(sys.stdin); print('yes' if 'PATHWAY-PRIORITY' in d else 'no')")

assert_eq "has VERSION" "yes" "$HAS_VERSION"
assert_eq "has TTL" "yes" "$HAS_TTL"
assert_eq "has RELOAD-URI" "yes" "$HAS_RELOAD"
assert_eq "has SERVICE-LOCATION-PRIORITY" "yes" "$HAS_SLP"
assert_eq "no PATHWAY-PRIORITY in DASH" "no" "$HAS_PP"

echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# Test 6: DASH session with multiple custom tokens
# ═══════════════════════════════════════════════════════════════════════════════

bold "── Test 6: DASH token passthrough ──"
echo ""

RESP_TOK=$(curl -s "$BASE/steer/dash?token=234523452&sid=session-xyz&_ss=$ENCODED_STATE")
TOK_URI=$(echo "$RESP_TOK" | python3 -c "import sys,json; print(json.load(sys.stdin).get('RELOAD-URI',''))")

assert_contains "token preserved" "$TOK_URI" "token=234523452"
assert_contains "sid preserved" "$TOK_URI" "sid=session-xyz"

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
