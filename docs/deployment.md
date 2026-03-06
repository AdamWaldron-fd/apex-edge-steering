# Deployment Guide

Platform-specific deployment instructions for Akamai EdgeWorkers, CloudFront Lambda@Edge, Cloudflare Workers, and Fastly Compute.

---

## Table of Contents

1. [Build the WASM Module](#build-the-wasm-module)
2. [Akamai EdgeWorkers](#akamai-edgeworkers) (primary target)
3. [CloudFront Lambda@Edge](#cloudfront-lambdaedge)
4. [Cloudflare Workers](#cloudflare-workers)
5. [Fastly Compute](#fastly-compute)
6. [Common Wrapper Pattern](#common-wrapper-pattern)
7. [Production Checklist](#production-checklist)

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

- [ ] Run all 101 tests: `cargo test`
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
