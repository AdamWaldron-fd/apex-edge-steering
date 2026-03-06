#!/usr/bin/env bash
#
# run-tests.sh — Build WASM, start local server, run all E2E test suites
#
# This is the main entry point for local testing. It:
#   1. Optionally rebuilds the WASM module (--build flag)
#   2. Runs cargo unit/integration tests
#   3. Starts the local steering server
#   4. Runs all E2E test scripts against it
#   5. Reports combined results
#
# Usage:
#   ./scripts/run-tests.sh           # Run E2E tests only (assumes WASM is built)
#   ./scripts/run-tests.sh --build   # Rebuild WASM first, then run all tests
#   ./scripts/run-tests.sh --all     # Run cargo tests + E2E tests
#   ./scripts/run-tests.sh --cargo   # Run cargo tests only

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PORT=3077
SERVER_PID=""

# ─── Helpers ──────────────────────────────────────────────────────────────────

red()   { printf "\033[31m%s\033[0m" "$1"; }
green() { printf "\033[32m%s\033[0m" "$1"; }
bold()  { printf "\033[1m%s\033[0m" "$1"; }

cleanup() {
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

# ─── Parse args ───────────────────────────────────────────────────────────────

RUN_BUILD=false
RUN_CARGO=false
RUN_E2E=true

for arg in "$@"; do
  case "$arg" in
    --build) RUN_BUILD=true ;;
    --cargo) RUN_CARGO=true; RUN_E2E=false ;;
    --all)   RUN_CARGO=true; RUN_BUILD=true ;;
    --help)
      echo "Usage: $0 [--build] [--cargo] [--all]"
      echo "  --build   Rebuild WASM before running E2E tests"
      echo "  --cargo   Run cargo tests only (no E2E)"
      echo "  --all     Run cargo tests + rebuild WASM + E2E tests"
      exit 0
      ;;
  esac
done

cd "$PROJECT_DIR"

echo ""
bold "╔══════════════════════════════════════════════╗"
bold "║       apex-steering test runner              ║"
bold "╚══════════════════════════════════════════════╝"
echo ""

# ─── Step 1: Cargo tests ─────────────────────────────────────────────────────

if [ "$RUN_CARGO" = true ]; then
  bold "── Step 1: Rust unit & integration tests ──"
  echo ""

  if [ -f "$HOME/.cargo/env" ]; then
    . "$HOME/.cargo/env"
  fi

  cargo test 2>&1
  CARGO_EXIT=$?

  if [ $CARGO_EXIT -ne 0 ]; then
    echo ""
    red "Cargo tests failed."
    echo ""
    exit $CARGO_EXIT
  fi

  echo ""
  green "All cargo tests passed."
  echo ""
fi

# ─── Step 2: WASM build ──────────────────────────────────────────────────────

if [ "$RUN_BUILD" = true ]; then
  bold "── Step 2: Building WASM module ──"
  echo ""

  if [ -f "$HOME/.cargo/env" ]; then
    . "$HOME/.cargo/env"
  fi

  wasm-pack build --target bundler --release 2>&1
  echo ""
  green "WASM build complete."
  echo ""
fi

# ─── Step 3: Verify WASM exists ──────────────────────────────────────────────

if [ "$RUN_E2E" = true ]; then
  if [ ! -f "$PROJECT_DIR/pkg/apex_steering_bg.wasm" ]; then
    echo ""
    red "ERROR: pkg/apex_steering_bg.wasm not found."
    echo "Run with --build to build the WASM module first, or run:"
    echo "  wasm-pack build --target bundler --release"
    echo ""
    exit 1
  fi

  WASM_SIZE=$(wc -c < "$PROJECT_DIR/pkg/apex_steering_bg.wasm" | tr -d ' ')
  echo "WASM binary: ${WASM_SIZE} bytes ($(( WASM_SIZE / 1024 )) KB)"
  echo ""

  # ─── Step 4: Start local server ──────────────────────────────────────────

  bold "── Starting local steering server on port $PORT ──"
  echo ""

  # Kill any existing process on the port
  lsof -ti:$PORT 2>/dev/null | xargs kill -9 2>/dev/null || true
  sleep 1

  node "$SCRIPT_DIR/server.mjs" --port $PORT &
  SERVER_PID=$!
  sleep 2

  # Verify server is up
  if ! curl -s "http://localhost:$PORT/health" > /dev/null 2>&1; then
    red "ERROR: Server failed to start on port $PORT"
    echo ""
    exit 1
  fi

  green "Server running (PID $SERVER_PID)"
  echo ""

  # ─── Step 5: Run E2E test suites ────────────────────────────────────────

  SUITE_PASS=0
  SUITE_FAIL=0

  run_suite() {
    local name="$1" script="$2"
    echo ""
    bold "════════════════════════════════════════════════"
    bold " Suite: $name"
    bold "════════════════════════════════════════════════"

    chmod +x "$script"
    if bash "$script" "http://localhost:$PORT"; then
      SUITE_PASS=$((SUITE_PASS + 1))
    else
      SUITE_FAIL=$((SUITE_FAIL + 1))
    fi
  }

  run_suite "HLS Client Sessions"   "$SCRIPT_DIR/test-hls-session.sh"
  run_suite "DASH Client Sessions"  "$SCRIPT_DIR/test-dash-session.sh"
  run_suite "Control Plane & QoE"   "$SCRIPT_DIR/test-control-plane.sh"

  # ─── Summary ──────────────────────────────────────────────────────────────

  echo ""
  bold "╔══════════════════════════════════════════════╗"
  bold "║               Final Summary                  ║"
  bold "╚══════════════════════════════════════════════╝"
  echo ""
  echo "  Test suites: $(green "$SUITE_PASS passed"), $([ $SUITE_FAIL -gt 0 ] && red "$SUITE_FAIL failed" || echo "$SUITE_FAIL failed")"
  echo ""

  if [ $SUITE_FAIL -gt 0 ]; then
    red "Some test suites failed."
    echo ""
    exit 1
  else
    green "All test suites passed."
    echo ""
  fi
fi
