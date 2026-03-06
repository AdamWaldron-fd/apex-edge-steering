# Deployment Guide

Local development server, E2E testing, and platform-specific deployment instructions.

---

## Table of Contents

1. [Local Development](#local-development)
2. [E2E Testing](#e2e-testing)
3. [Build the WASM Module](#build-the-wasm-module)
4. [Akamai EdgeWorkers](#akamai-edgeworkers) (primary target)
5. [CloudFront Lambda@Edge](#cloudfront-lambdaedge)
6. [Cloudflare Workers](#cloudflare-workers)
7. [Fastly Compute](#fastly-compute)
8. [Common Wrapper Pattern](#common-wrapper-pattern)
9. [Production Checklist](#production-checklist)

---

## Local Development

A local HTTP server is included for development, POC demos, and player integration testing. It loads the WASM module directly from `pkg/` and serves all steering endpoints on a configurable port.

### Prerequisites

- Node.js 18+
- WASM module built in `pkg/` (run `wasm-pack build --target bundler --release`)

### Start the Server

```bash
# Default port 3001
node scripts/server.mjs

# Custom port
node scripts/server.mjs --port 8080
```

```
apex-steering dev server listening on http://localhost:3001

Endpoints:
  GET  /                       Dev UI
  GET  /steer[/hls|/dash]?...  Steering requests
  POST /control                Master control commands
  GET  /health                 Health check
  POST /config                 Update policy config
  POST /encode-state           Encode initial session state
  POST /reset                  Reset overrides and config

  Dev UI: http://localhost:3001/
```

### Dev UI

Open `http://localhost:3001/` in a browser for interactive testing. The UI provides:

- **Steering tab** — Build and send HLS/DASH steering requests. RELOAD-URI tracking lets you
  follow a multi-hop session by clicking "Use" to load the `_ss` from the previous response.
- **Control tab** — Send `set_priorities`, `exclude_pathway`, and `clear_overrides` commands
  with auto-incrementing generation numbers.
- **Config tab** — Read and update `PolicyConfig` (TTL, QoE settings) at runtime.
- **Encode tab** — Encode initial session state and load it directly into the Steering tab.
- **Response panel** — Syntax-highlighted JSON responses with status codes and timing.
- **Log panel** — Rolling request log of all API calls.

The UI is a single HTML file (`scripts/ui.html`) with zero dependencies, served directly by
the dev server.

### Dev-Only Endpoints

The local server includes convenience endpoints not present in production wrappers:

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/encode-state` | POST | Encode a `SessionState` JSON into a base64 `_ss` string and store it on the edge server as fallback for requests without `_ss`. Simulates what the master steering server does. |
| `/config` | POST | Update the `PolicyConfig` at runtime without restarting. |
| `/config` | GET | Read the current policy config. |
| `/reset` | POST | Clear all overrides, config, and stored initial state back to defaults. |

### Walkthrough: Complete HLS Session

```bash
# 1. Encode initial session state (manifest updater step)
ENCODE=$(curl -s -X POST http://localhost:3001/encode-state \
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
echo "$ENCODE" | python3 -m json.tool

# Extract the encoded state
SS=$(echo "$ENCODE" | python3 -c "import sys,json; print(json.load(sys.stdin)['encoded'])")

# 2. First HLS steering request (simulates player's first call)
curl -s "http://localhost:3001/steer/hls?session=abc&_ss=$SS" | python3 -m json.tool
# Response:
# {
#   "VERSION": 1,
#   "TTL": 300,
#   "RELOAD-URI": "/steer?session=abc&_ss=...",
#   "PATHWAY-PRIORITY": ["cdn-a", "cdn-b"]
# }

# 3. Follow-up with pathway and throughput (player reports current CDN + speed)
# Use the RELOAD-URI query from step 2, append _HLS_pathway and _HLS_throughput
curl -s "http://localhost:3001/steer/hls?session=abc&_ss=$SS&_HLS_pathway=cdn-a&_HLS_throughput=5140000" \
  | python3 -m json.tool

# 4. Push a master override (simulate master server)
curl -s -X POST http://localhost:3001/control \
  -H "Content-Type: application/json" \
  -d '{
    "type": "set_priorities",
    "region": null,
    "priorities": ["cdn-b", "cdn-a"],
    "generation": 1,
    "ttl_override": 30
  }' | python3 -m json.tool

# 5. Next steering request picks up the override
curl -s "http://localhost:3001/steer/hls?session=abc&_ss=$SS&_HLS_pathway=cdn-a&_HLS_throughput=5140000" \
  | python3 -m json.tool
# Response now has:
#   "PATHWAY-PRIORITY": ["cdn-b", "cdn-a"],
#   "TTL": 30

# 6. Check health (shows current override state)
curl -s http://localhost:3001/health | python3 -m json.tool

# 7. Reset everything
curl -s -X POST http://localhost:3001/reset | python3 -m json.tool
```

### Walkthrough: DASH queryBeforeStart Session

```bash
# 1. Encode state for DASH content
SS=$(curl -s -X POST http://localhost:3001/encode-state \
  -H "Content-Type: application/json" \
  -d '{"priorities":["alpha","beta"],"min_bitrate":500000,"max_bitrate":6000000}' \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['encoded'])")

# 2. First request: queryBeforeStart=true (no _DASH_ params yet)
curl -s "http://localhost:3001/steer/dash?token=234523452&_ss=$SS" | python3 -m json.tool
# Response:
# {
#   "VERSION": 1,
#   "TTL": 300,
#   "RELOAD-URI": "/steer?token=234523452&_ss=...",
#   "SERVICE-LOCATION-PRIORITY": ["alpha", "beta"]
# }

# 3. Follow-up with DASH pathway (note: double-quoted per spec, but unquoted works too)
curl -s "http://localhost:3001/steer/dash?token=234523452&_ss=$SS&_DASH_pathway=alpha&_DASH_throughput=5140000" \
  | python3 -m json.tool
```

### Walkthrough: QoE CDN Switching

```bash
# Setup: state with known encoding ladder
SS=$(curl -s -X POST http://localhost:3001/encode-state \
  -H "Content-Type: application/json" \
  -d '{"priorities":["cdn-a","cdn-b"],"min_bitrate":1000000,"max_bitrate":8000000}' \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['encoded'])")

# Good throughput: cdn-a stays on top, TTL=300
curl -s "http://localhost:3001/steer/hls?_ss=$SS&_HLS_pathway=cdn-a&_HLS_throughput=5000000" \
  | python3 -m json.tool

# Degraded throughput (500K < 1.2 * 1M threshold): cdn-a demoted, TTL=10
curl -s "http://localhost:3001/steer/hls?_ss=$SS&_HLS_pathway=cdn-a&_HLS_throughput=500000" \
  | python3 -m json.tool
# Response:
#   "PATHWAY-PRIORITY": ["cdn-b", "cdn-a"],  <-- cdn-b promoted
#   "TTL": 10                                 <-- fast re-evaluation
```

### Walkthrough: Disaster Recovery

```bash
# Exclude a CDN (outage detected by master)
curl -s -X POST http://localhost:3001/control \
  -H "Content-Type: application/json" \
  -d '{"type":"exclude_pathway","region":null,"pathway":"cdn-a","generation":1}'

# Steering responses no longer include cdn-a
curl -s "http://localhost:3001/steer/hls?_ss=$SS" | python3 -m json.tool

# CDN recovered — clear all overrides
curl -s -X POST http://localhost:3001/control \
  -H "Content-Type: application/json" \
  -d '{"type":"clear_overrides","region":null,"generation":2}'
```

### Server Architecture

```
┌──────────────────────────────────────────────────────────────┐
│  Local Dev Server (scripts/server.mjs)                        │
│                                                               │
│  Node.js HTTP server + WASM loader                            │
│                                                               │
│  ┌───────────────────────────────────────────────────────┐   │
│  │  WASM Module (pkg/apex_steering_bg.wasm)               │   │
│  │  Loaded via WebAssembly.instantiate() at startup       │   │
│  │                                                        │   │
│  │  Exports:                                              │   │
│  │    handle_steering_request()                           │   │
│  │    parse_request()                                     │   │
│  │    apply_control_command()                             │   │
│  │    encode_initial_state()  (also stores state)         │   │
│  │    reset_initial_state()   (clears stored state)       │   │
│  └───────────────────────────────────────────────────────┘   │
│                                                               │
│  In-memory state:                                             │
│    overridesJson  (updated via POST /control)                 │
│    configJson     (updated via POST /config)                  │
│                                                               │
│  Routes:                                                      │
│    GET  /            → Dev UI (scripts/ui.html)                │
│    GET  /steer/**    → parse_request + handle_steering_request│
│    POST /control     → apply_control_command                  │
│    GET  /health      → status + current overrides             │
│    POST /encode-state→ encode_initial_state (dev only)        │
│    POST /config      → update PolicyConfig (dev only)         │
│    POST /reset       → clear all state (dev only)             │
└──────────────────────────────────────────────────────────────┘
```

---

## E2E Testing

Three test suites validate the full HTTP request/response cycle against the live WASM server. Tests use cURL and Python for JSON extraction.

### Run All Tests

```bash
# Start server, run all 98 E2E tests, stop server
./scripts/run-tests.sh

# Full pipeline: cargo tests + WASM build + E2E
./scripts/run-tests.sh --all

# Cargo tests only
./scripts/run-tests.sh --cargo
```

### Run Individual Suites

With the server already running (`node scripts/server.mjs`):

```bash
./scripts/test-hls-session.sh http://localhost:3001     # 27 tests
./scripts/test-dash-session.sh http://localhost:3001     # 22 tests
./scripts/test-control-plane.sh http://localhost:3001    # 49 tests
```

### Test Suites

**HLS Client Sessions** (`test-hls-session.sh`) -- 27 tests:
- Initial state encoding via `/encode-state`
- First request with no pathway (session start)
- Follow-up with `_HLS_pathway` + `_HLS_throughput`
- State accumulation across 3 requests
- Akamai EdgeAuth token passthrough (4 tokens, verified across 2 hops)
- Protocol auto-detection from `_HLS_*` query params
- HLS response JSON format: `VERSION`, `TTL`, `RELOAD-URI`, `PATHWAY-PRIORITY` present; `SERVICE-LOCATION-PRIORITY` absent

**DASH Client Sessions** (`test-dash-session.sh`) -- 22 tests:
- `queryBeforeStart` first request (no `_DASH_*` params)
- Follow-up with `_DASH_pathway` + `_DASH_throughput`
- Double-quoted `_DASH_pathway` (`%22alpha%22`) per DASH-IF spec
- Protocol auto-detection from `_DASH_*` query params
- DASH response JSON format: `SERVICE-LOCATION-PRIORITY` present; `PATHWAY-PRIORITY` absent
- Token passthrough for DASH sessions

**Control Plane + QoE** (`test-control-plane.sh`) -- 49 tests:
- `set_priorities`: command accepted, affects steering responses, TTL override
- Stale command rejection: generation-based idempotency (equal and lower gen rejected)
- `exclude_pathway`: CDN removed from responses
- `clear_overrides`: priorities and exclusions restored
- Malformed command returns HTTP 400
- QoE demotion: degraded throughput (500K < 1.2M threshold) demotes top CDN, TTL=10
- QoE healthy: good throughput keeps normal priorities and TTL=300
- QoE edge cases: exactly at threshold (not degraded), just below (degraded)
- QoE full cycle: good --> degraded --> recovered (TTL 300 --> 10 --> 300)
- Master + QoE interaction: QoE demotes even with active master override
- Disaster recovery: exclude --> verify removed --> clear --> verify restored
- Master override precedence: override takes effect over client state across multi-hop
  HLS and DASH sessions, new override replaces old mid-session

---

## Build the WASM Module

All platforms start from the same WASM build. Choose the target based on your platform:

```bash
# Prerequisites
rustup target add wasm32-unknown-unknown
cargo install wasm-pack

# Build for JS bundler environments (Akamai, general)
wasm-pack build --target bundler --release

# Build for Node.js (Lambda@Edge)
wasm-pack build --target nodejs --release

# Build for web (Cloudflare Workers)
wasm-pack build --target web --release
```

Output in `pkg/`:

```
pkg/
├── apex_steering_bg.wasm    ~198 KB WASM binary
├── apex_steering_bg.js      JS glue code
├── apex_steering.js          ES module entry
├── apex_steering.d.ts        TypeScript declarations
└── package.json              npm metadata
```

Note: `wasm-opt` is disabled in `Cargo.toml` because the bundled version doesn't support bulk memory operations from modern Rust. Rust's own LTO and size optimization handle binary optimization.

---

## Akamai EdgeWorkers

apex-steering is designed with Akamai as the primary deployment target.

### Files

```
wrappers/akamai/
├── main.js          onClientRequest handler
└── bundle.json      EdgeWorker metadata + path matching
```

### Bundle Creation

```bash
# 1. Build WASM
wasm-pack build --target bundler --release

# 2. Create bundle directory
mkdir -p bundle
cp wrappers/akamai/main.js bundle/
cp wrappers/akamai/bundle.json bundle/
cp pkg/apex_steering_bg.wasm bundle/
cp pkg/apex_steering_bg.js bundle/
cp pkg/apex_steering.js bundle/

# 3. Package as tarball for upload
cd bundle && tar -czf ../apex-steering-edgeworker.tgz . && cd ..
```

### Path Matching

Configured in `bundle.json`:

```json
{
  "edgeworker-version": "0.1.0",
  "description": "apex-steering: Stateless HLS/DASH Content Steering edge server",
  "match-rules": {
    "OR": [
      { "matches": [{ "name": "path", "value": "/steer/*" }] },
      { "matches": [{ "name": "path", "value": "/control" }] },
      { "matches": [{ "name": "path", "value": "/health" }] }
    ]
  }
}
```

### Deploy

```bash
# Using Akamai CLI
akamai edgeworkers upload --bundle apex-steering-edgeworker.tgz --ewid <EDGEWORKER_ID>
akamai edgeworkers activate --ewid <EDGEWORKER_ID> --network staging
# After validation:
akamai edgeworkers activate --ewid <EDGEWORKER_ID> --network production
```

Or deploy via Akamai Control Center UI.

### Handler Architecture

```
┌──────────────────────────────────────────────────┐
│  Akamai EdgeWorker (main.js)                      │
│                                                    │
│  onClientRequest(request)                          │
│    │                                               │
│    ├─ /health ──────> 200 {"status":"ok"}          │
│    │                                               │
│    ├─ /control (POST) ──> apply_control_command()  │
│    │                      update in-memory state    │
│    │                      return updated overrides  │
│    │                                               │
│    └─ /steer/** ──────> parse_request()            │
│                         handle_steering_request()   │
│                         return JSON response        │
│                                                    │
│  State: overridesJson (in-memory, per-instance)    │
│  Config: configJson (loaded from EdgeKV or inline) │
└──────────────────────────────────────────────────┘
```

### WASM Size

The 198 KB WASM binary is well within Akamai's EdgeWorker bundle size limits.

---

## CloudFront Lambda@Edge

### Files

```
wrappers/cloudfront/
└── index.js    Lambda@Edge viewer-request handler
```

### Build & Package

```bash
# 1. Build for Node.js
wasm-pack build --target nodejs --release

# 2. Create deployment package
mkdir -p lambda-package
cp wrappers/cloudfront/index.js lambda-package/
cp pkg/apex_steering_bg.wasm lambda-package/
cp pkg/apex_steering.js lambda-package/
cp pkg/apex_steering_bg.js lambda-package/

# 3. Create zip for Lambda
cd lambda-package && zip -r ../apex-steering-lambda.zip . && cd ..
```

### Deploy

```bash
# Create Lambda function
aws lambda create-function \
  --function-name apex-steering \
  --runtime nodejs18.x \
  --handler index.handler \
  --zip-file fileb://apex-steering-lambda.zip \
  --role arn:aws:iam::123456789:role/lambda-edge-role

# Publish version (required for Lambda@Edge)
aws lambda publish-version --function-name apex-steering

# Associate with CloudFront distribution as viewer-request trigger
```

### CloudFront Configuration

- **Event type:** Viewer Request
- **Path pattern:** `/steer/*`, `/control`, `/health`
- **Origin:** Not needed (function generates response directly)

### Handler Architecture

```
CloudFront Event
  │
  v
exports.handler(event)
  │
  ├─ event.Records[0].cf.request
  │    uri: "/steer/hls"
  │    querystring: "_ss=...&_HLS_pathway=cdn-a"
  │
  ├─ /health ──> return { status: '200', body: '{"status":"ok"}' }
  ├─ /control ──> apply_control_command() -> return updated state
  └─ /steer/** ──> parse_request() -> handle_steering_request() -> return response
```

---

## Cloudflare Workers

### Files

```
wrappers/cloudflare/
└── worker.js    Workers fetch handler (ES module format)
```

### Deploy with Wrangler

```toml
# wrangler.toml
name = "apex-steering"
main = "worker.js"
compatibility_date = "2024-01-01"

[build]
command = "wasm-pack build --target web --release"

[[rules]]
type = "CompiledWasm"
globs = ["**/*.wasm"]
```

```bash
# Build and deploy
wasm-pack build --target web --release
wrangler deploy
```

### Handler Architecture

```
fetch(request, env, ctx)
  │
  ├─ new URL(request.url)
  │    pathname: "/steer/hls"
  │    search: "?_ss=...&_HLS_pathway=cdn-a"
  │
  ├─ /health ──> new Response('{"status":"ok"}')
  ├─ /control ──> apply_control_command() -> new Response(overrides)
  ├─ /steer/** ──> parse_request() -> handle_steering_request() -> new Response(json)
  └─ else ──> new Response('Not Found', { status: 404 })
```

---

## Fastly Compute

### Files

```
wrappers/fastly/
└── index.js    Compute fetch event handler
```

### Deploy

```bash
# Build for bundler target
wasm-pack build --target bundler --release

# Package with Fastly CLI
fastly compute init  # if not already initialized
fastly compute build
fastly compute deploy
```

### Handler Architecture

Same pattern as Cloudflare Workers -- `addEventListener('fetch', ...)` with routing to WASM functions.

---

## Common Wrapper Pattern

All four platform wrappers follow the same thin pattern (~80-100 lines each):

```javascript
// 1. Import WASM functions
import { handle_steering_request, parse_request, apply_control_command } from './pkg/apex_steering';

// 2. In-memory state (per worker instance)
let overridesJson = '';
let configJson = '';
const BASE_PATH = '/steer';

// 3. Route requests
async function handleRequest(request) {
  const path = /* extract path */;
  const query = /* extract query string */;

  // Health check
  if (path === '/health') {
    return respond(200, { status: 'ok', engine: 'apex-steering' });
  }

  // Control plane
  if (path === '/control' && method === 'POST') {
    const body = await request.text();
    overridesJson = apply_control_command(overridesJson, body);
    return respond(200, JSON.parse(overridesJson));
  }

  // Steering
  if (path.startsWith('/steer')) {
    const protocol = detectProtocol(path, query);
    const requestJson = parse_request(query, protocol);
    const responseJson = handle_steering_request(
      requestJson, overridesJson, configJson, BASE_PATH
    );
    return respond(200, JSON.parse(responseJson), {
      'Cache-Control': 'no-store, no-cache',
      'Access-Control-Allow-Origin': '*',
    });
  }
}

// 4. Protocol detection from path or query params
function detectProtocol(path, query) {
  if (path.includes('/hls')) return 'hls';
  if (path.includes('/dash')) return 'dash';
  if (query.includes('_HLS_')) return 'hls';
  if (query.includes('_DASH_')) return 'dash';
  return 'hls'; // default
}
```

---

## Production Checklist

### Before Deploy

- [ ] Run all Rust tests: `cargo test` (109 tests)
- [ ] Run E2E tests: `./scripts/run-tests.sh` (98 tests)
- [ ] Build WASM for target platform
- [ ] Verify WASM binary size (~198 KB)
- [ ] Configure `base_path` to match your routing
- [ ] Set up CDN path matching for `/steer/*`, `/control`, `/health`

### Response Headers

All steering responses must include:

```
Content-Type: application/json
Cache-Control: no-store, no-cache
Access-Control-Allow-Origin: *
```

`no-store, no-cache` prevents intermediate caches from serving stale steering responses. `Access-Control-Allow-Origin: *` is required because players make cross-origin requests to the steering endpoint.

### Override State Persistence

Override state is **in-memory** per worker instance:
- New instances start with empty overrides
- Master server should push active overrides via heartbeat (every 60s)
- Alternatively, load from EdgeKV (Akamai), DynamoDB (CloudFront), or KV (Cloudflare) on worker init

### Monitoring

- Health check: `GET /health` returns `{"status":"ok","engine":"apex-steering"}`
- Override state: inspect response from `POST /control`
- TTL=10 in responses indicates active QoE optimization (CDN degradation detected)

### Scaling

The WASM core is a pure function with no I/O. It scales linearly with edge compute instances. Typical latency: <1ms per steering request (decode + policy + encode).
