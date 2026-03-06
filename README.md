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

### Build & Test

```bash
# Run all 101 tests (91 unit + 10 integration)
cargo test

# Build WASM for bundler environments (Akamai, general)
wasm-pack build --target bundler --release

# Build for Node.js (Lambda@Edge)
wasm-pack build --target nodejs --release

# Build for web (Cloudflare Workers)
wasm-pack build --target web --release
```

Output: `pkg/` directory (~198 KB `.wasm` + JS glue + TypeScript declarations).

---

## Documentation

| Document | Description |
|----------|-------------|
| [Architecture & Design](docs/architecture.md) | System design, stateless model, session state encoding, data flow diagrams |
| [Client Usage Guide](docs/client-usage.md) | HLS and DASH integration examples for player developers |
| [Control Plane Guide](docs/control-plane.md) | Master server integration, override commands, disaster recovery |
| [API Reference](docs/api-reference.md) | HTTP endpoints, WASM exports, TypeScript declarations, JSON schemas |
| [Configuration & Tuning](docs/configuration.md) | Policy config, QoE parameters, TTL tuning |
| [Deployment Guide](docs/deployment.md) | Platform-specific deployment for Akamai, CloudFront, Cloudflare, Fastly |

---

## Project Layout

```
apex-steering/
├── Cargo.toml                 Project config (cdylib + rlib)
├── README.md                  This file
├── CLAUDE.md                  AI assistant context
│
├── src/
│   ├── lib.rs                 WASM entry points (4 exports)
│   ├── types.rs               All type definitions
│   ├── state.rs               Session state encode/decode, query parsing
│   ├── policy.rs              CDN selection policy engine
│   ├── response.rs            Steering response construction
│   └── control.rs             Master-to-edge override handling
│
├── tests/
│   └── integration.rs         10 end-to-end integration tests
│
├── wrappers/
│   ├── akamai/                EdgeWorkers (primary target)
│   ├── cloudfront/            Lambda@Edge
│   ├── cloudflare/            Workers
│   └── fastly/                Compute@Edge
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

**101 tests**, all passing.

| Module | Unit Tests | Coverage |
|--------|-----------|----------|
| `state.rs` | 30 | Encode/decode roundtrips, query parsing, URL decoding, RELOAD-URI construction |
| `policy.rs` | 24 | Priority logic, master overrides, pathway exclusions, QoE optimization |
| `control.rs` | 19 | Command processing, generation idempotency, JSON deserialization |
| `response.rs` | 18 | Response building, state propagation, token passthrough |
| `integration.rs` | 10 | Full session lifecycles, QoE switching, disaster recovery, concurrent viewers |

---

## Reference Specifications

- [HLS Content Steering Specification v1.2b1](https://developer.apple.com/streaming/HLSContentSteeringSpecification.pdf) (Apple, 2021)
- [DASH-IF Content Steering CTS 00XX v0.9.0](https://dashif.org/docs/DASH-IF-CTS-00XX-Content-Steering-Community-Review.pdf) (DASH-IF, 2022)
- [Implementing HLS/DASH Content Steering at Scale](https://www.ibc.org/technical-papers/implementing-hls-dash-content-steering-at-scale/10567.article) (Reznik et al., Brightcove, IBC2023)

## License

MIT
