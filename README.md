# apex-steering

Stateless edge content steering server for HLS and DASH streaming.

Implements both [Apple HLS Content Steering](https://developer.apple.com/streaming/HLSContentSteeringSpecification.pdf) (v1.2b1) and [DASH-IF Content Steering](https://dashif.org/docs/DASH-IF-CTS-00XX-Content-Steering-Community-Review.pdf) (CTS 00XX v0.9.0). Written in Rust, compiled to WASM for deployment on any edge compute platform.

---

## Architecture

This is the **edge steering server** from Figure 3 of [Implementing HLS/DASH Content Steering at Scale](https://www.ibc.org/technical-papers/implementing-hls-dash-content-steering-at-scale/10567.article) (Reznik et al., Brightcove, IBC2023).

```
                     ┌─────────────────────────────────────────────┐
                     │             Steering Master                 │
                     │  (load balancing, COGS, contract mgmt)      │
                     └──────┬──────────────────────┬───────────────┘
                            │                      │
                   initial CDN order         POST /control
                   (per new session)         (forced overrides)
                            │                      │
                            v                      v
┌──────────────────┐   ┌────────────┐   ┌─────────────────────┐
│  Origin / CMS    │──>│  Manifest  │   │  Edge Steering      │
│                  │   │  Updater   │   │  Server (this)      │
└──────────────────┘   │            │   │  ┌───────────────┐  │
                       └──────┬─────┘   │  │ Rust -> WASM  │  │
                              │         │  │  198 KB core  │  │
                     embeds SERVER-URI  │  └───────────────┘  │
                     with initial state │  Akamai / CF / CFW  │
                              │         └──────────▲──────────┘
                              v                    │
                       ┌──────────────┐    TTL-based polling
                       │  HLS / DASH  │    (_HLS_* / _DASH_*)
                       │  Manifest    │            │
                       │  (on CDN)    │    ┌───────┴──────────┐
                       └──────┬───────┘    │  Player          │
                              │            │  AVPlayer, HLS.js│
                              └───────────>│  DASH.js, Shaka  │
                                           └──────────────────┘
```

**Key property: fully stateless.** All session context is carried in URL parameters. No database, no cache, no session store. The server is a pure function deployable at CDN scale.

---

## How It Works

```
 Player                     Edge Server                     Master
   │                            │                              │
   │  GET /steer?_ss=<state>    │                              │
   │  &_HLS_pathway=cdn-a       │                              │
   │  &_HLS_throughput=5140000  │                              │
   │ ─────────────────────────> │                              │
   │                            │  decode _ss                  │
   │                            │  check overrides             │
   │                            │  evaluate QoE policy         │
   │                            │  encode updated _ss          │
   │  {                         │                              │
   │    "VERSION": 1,           │                              │
   │    "TTL": 300,             │                              │
   │    "RELOAD-URI": "...",    │                              │
   │    "PATHWAY-PRIORITY":     │                              │
   │      ["cdn-a", "cdn-b"]    │                              │
   │  }                         │                              │
   │ <───────────────────────── │                              │
   │                            │                              │
   │  (wait TTL seconds)        │  POST /control               │
   │                            │  {"type":"set_priorities",...}│
   │                            │ <─────────────────────────── │
   │                            │                              │
   │  GET /steer?_ss=<state>    │                              │
   │ ─────────────────────────> │  (applies master override)   │
   │  { "PATHWAY-PRIORITY":     │                              │
   │    ["cdn-b", "cdn-a"] }    │                              │
   │ <───────────────────────── │                              │
```

1. **Session Start** -- Manifest updater embeds `SERVER-URI` (HLS) or `<ContentSteering>` (DASH) with encoded initial state
2. **Steering Loop** -- Player polls at `TTL` intervals, sending current pathway and throughput
3. **Decision** -- Edge server decodes state, applies overrides, runs QoE policy, returns updated priorities
4. **CDN Switch** -- Player seamlessly switches to new top-priority CDN at next segment boundary
5. **Control** -- Master server can push overrides at any time via `POST /control`

---

## Quick Start

### Prerequisites

- Rust toolchain ([rustup](https://rustup.rs/))
- `wasm32-unknown-unknown` target: `rustup target add wasm32-unknown-unknown`
- `wasm-pack`: `cargo install wasm-pack`
- Node.js 18+ (for local dev server and E2E tests)

### Build & Test

```bash
# Run all 109 Rust unit + integration tests
cargo test

# Build WASM module
wasm-pack build --target bundler --release
```

Output: `pkg/` directory (~198 KB `.wasm` + JS glue + TypeScript declarations).

### Run Locally

Start a local steering server for development, POC testing, or player integration:

```bash
# Start the dev server (default port 3001)
node scripts/server.mjs

# Or specify a port
node scripts/server.mjs --port 8080
```

The server loads the WASM module from `pkg/` and exposes all steering endpoints.
A browser-based dev UI is available at `http://localhost:3001/` for interactive testing.

```
  GET  /                       Dev UI (browser)
  GET  /steer[/hls|/dash]?...  Steering requests (player-facing)
  POST /control                Master control commands
  GET  /health                 Health check
  POST /encode-state           Encode initial session state (manifest updater)
  POST /config                 Update policy config at runtime
  POST /reset                  Reset all overrides and config
```

Try it:

```bash
# 1. Encode initial session state (what the manifest updater does)
curl -s -X POST http://localhost:3001/encode-state \
  -H "Content-Type: application/json" \
  -d '{"priorities":["cdn-a","cdn-b"],"min_bitrate":783322,"max_bitrate":4530860}'

# 2. Make an HLS steering request using the encoded state
curl -s "http://localhost:3001/steer/hls?_ss=<encoded>&_HLS_pathway=cdn-a&_HLS_throughput=5140000"

# 3. Push a master override
curl -s -X POST http://localhost:3001/control \
  -H "Content-Type: application/json" \
  -d '{"type":"set_priorities","region":null,"priorities":["cdn-b","cdn-a"],"generation":1,"ttl_override":30}'

# 4. Check health and current override state
curl -s http://localhost:3001/health
```

See [Local Development](docs/deployment.md#local-development) for the full walkthrough.

### Run E2E Tests

Run the full end-to-end test suite against the live WASM server:

```bash
# Run all 98 E2E tests (starts server automatically)
./scripts/run-tests.sh

# Run everything: cargo tests + WASM rebuild + E2E tests
./scripts/run-tests.sh --all

# Run individual test suites (server must be running)
./scripts/test-hls-session.sh       # 27 HLS client tests
./scripts/test-dash-session.sh      # 22 DASH client tests
./scripts/test-control-plane.sh     # 49 control plane + QoE tests
```

---

## Documentation

| Document | Description |
|----------|-------------|
| [Architecture & Design](docs/architecture.md) | System design, stateless model, session state encoding, data flow diagrams |
| [Client Usage Guide](docs/client-usage.md) | HLS and DASH integration examples for player developers |
| [Control Plane Guide](docs/control-plane.md) | Master server integration, override commands, disaster recovery |
| [API Reference](docs/api-reference.md) | HTTP endpoints, WASM exports, TypeScript declarations, JSON schemas |
| [Configuration & Tuning](docs/configuration.md) | Policy config, QoE parameters, TTL tuning |
| [Deployment Guide](docs/deployment.md) | Local dev server, Akamai, CloudFront, Cloudflare, Fastly |

---

## Project Layout

```
apex-steering/
├── Cargo.toml                 Project config (cdylib + rlib)
├── README.md                  This file
├── CLAUDE.md                  AI assistant context
│
├── src/
│   ├── lib.rs                 WASM entry points (5 exports), initial state storage
│   ├── types.rs               All type definitions
│   ├── state.rs               Session state encode/decode, query parsing
│   ├── policy.rs              CDN selection policy engine
│   ├── response.rs            Steering response construction
│   └── control.rs             Master-to-edge override handling
│
├── tests/
│   └── integration.rs         12 end-to-end integration tests
│
├── wrappers/
│   ├── akamai/                EdgeWorkers (primary target)
│   ├── cloudfront/            Lambda@Edge
│   ├── cloudflare/            Workers
│   └── fastly/                Compute@Edge
│
├── scripts/
│   ├── server.mjs             Local dev server (loads WASM, serves HTTP + dev UI)
│   ├── ui.html                Browser-based dev UI (served at /)
│   ├── run-tests.sh           Test orchestrator (--build, --cargo, --all)
│   ├── test-hls-session.sh    27 HLS client E2E tests
│   ├── test-dash-session.sh   22 DASH client E2E tests
│   └── test-control-plane.sh  49 control plane + QoE E2E tests
│
├── docs/                      Documentation
│   ├── architecture.md
│   ├── client-usage.md
│   ├── control-plane.md
│   ├── api-reference.md
│   ├── configuration.md
│   └── deployment.md
│
└── pkg/                       Build output (gitignored)
    ├── apex_steering_bg.wasm  WASM binary (~198 KB)
    ├── apex_steering.js       ES module entry
    ├── apex_steering.d.ts     TypeScript declarations
    └── package.json           npm metadata
```

---

## Supported Protocols

| | HLS Content Steering | DASH Content Steering |
|---|---|---|
| **Spec** | Apple v1.2b1 | DASH-IF CTS 00XX v0.9.0 |
| **Manifest** | `#EXT-X-CONTENT-STEERING` | `<ContentSteering>` MPD element |
| **Client params** | `_HLS_pathway`, `_HLS_throughput` | `_DASH_pathway`, `_DASH_throughput` |
| **Response key** | `PATHWAY-PRIORITY` | `SERVICE-LOCATION-PRIORITY` |
| **Player support** | AVPlayer, HLS.js | DASH.js, Shaka Player |

---

## Test Coverage

**207 tests total** (109 Rust + 98 E2E), all passing.

### Rust Tests (109)

| Module | Tests | Coverage |
|--------|-------|----------|
| `state.rs` | 30 | Encode/decode roundtrips, query parsing, URL decoding, RELOAD-URI construction |
| `policy.rs` | 28 | Priority logic, master overrides, pathway exclusions, QoE optimization, master precedence over client state |
| `control.rs` | 19 | Command processing, generation idempotency, JSON deserialization |
| `response.rs` | 20 | Response building, state propagation, token passthrough, override persistence in RELOAD-URI |
| `integration.rs` | 12 | Full session lifecycles, QoE switching, disaster recovery, concurrent viewers, multi-hop override persistence |

### E2E Tests (98)

| Suite | Tests | Coverage |
|-------|-------|----------|
| HLS Sessions | 27 | State encoding, multi-request sessions, Akamai token passthrough, protocol auto-detection, JSON format validation |
| DASH Sessions | 22 | queryBeforeStart, double-quoted pathways, SERVICE-LOCATION-PRIORITY format, token passthrough |
| Control Plane + QoE | 49 | set/exclude/clear commands, stale rejection, QoE demotion, threshold edge cases, full degradation cycle, master+QoE interaction, disaster recovery, master override precedence across multi-hop HLS and DASH sessions |

---

## Reference Specifications

- [HLS Content Steering Specification v1.2b1](https://developer.apple.com/streaming/HLSContentSteeringSpecification.pdf) (Apple, 2021)
- [DASH-IF Content Steering CTS 00XX v0.9.0](https://dashif.org/docs/DASH-IF-CTS-00XX-Content-Steering-Community-Review.pdf) (DASH-IF, 2022)
- [Implementing HLS/DASH Content Steering at Scale](https://www.ibc.org/technical-papers/implementing-hls-dash-content-steering-at-scale/10567.article) (Reznik et al., Brightcove, IBC2023)

