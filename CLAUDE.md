# apex-steering

Stateless edge content steering server implementing both
[HLS Content Steering](https://developer.apple.com/streaming/HLSContentSteeringSpecification.pdf)
(Apple spec v1.2b1) and
[DASH Content Steering](https://dashif.org/docs/DASH-IF-CTS-00XX-Content-Steering-Community-Review.pdf)
(DASH-IF CTS 00XX v0.9.0).

Written in Rust, compiled to WASM for deployment on edge compute platforms.

---

## Table of Contents

1. [Architecture](#architecture)
2. [How It Works](#how-it-works)
3. [Protocol Support](#protocol-support)
4. [Stateless Design](#stateless-design)
5. [CDN Token Passthrough](#cdn-token-passthrough)
6. [API Reference](#api-reference)
7. [WASM API Reference](#wasm-api-reference)
8. [QoE Optimization](#qoe-optimization)
9. [Control Plane](#control-plane)
10. [Deployment](#deployment)
11. [Build & Test](#build--test)
12. [Project Layout](#project-layout)
13. [Test Coverage](#test-coverage)
14. [Reference Specifications](#reference-specifications)

---

## Architecture

This project implements the **edge steering server** from Figure 3 of the IBC2023 paper
"Implementing HLS/DASH Content Steering at Scale" (Reznik et al., Brightcove). The
architecture splits steering operations into two stages:

```
                    ┌──────────────────────────────────────────────┐
                    │              Steering Master                  │
                    │  (load balancing, COGS, contract management)  │
                    └──────┬───────────────────────┬───────────────┘
                           │                       │
                  initial CDN order          POST /control
                  (per new session)          (forced overrides)
                           │                       │
                           ▼                       ▼
                    ┌──────────────┐        ┌──────────────────┐
                    │   Manifest   │        │  Edge Steering   │
                    │   Updater    │        │  Server (this)   │
                    │              │        │  [WASM @ Edge]   │
                    └──────┬───────┘        └────────▲─────────┘
                           │                         │
                  embeds SERVER-URI            TTL-based polling
                  with initial state           (_HLS_* / _DASH_*)
                           │                         │
                           ▼                         │
                    ┌──────────────┐        ┌────────┴─────────┐
                    │  HLS / DASH  │───────▶│  HLS / DASH      │
                    │  Manifest    │        │  Player           │
                    │  (CDN)       │        │  (AVPlayer,       │
                    └──────────────┘        │   HLS.js, DASH.js)│
                                            └──────────────────┘
```

### Role of Each Component

| Component | Responsibility | Statefulness |
|-----------|---------------|-------------|
| **Steering Master** | Global/regional CDN decisions: load balancing, COGS optimization, contract commit management, disaster recovery initiation | Stateful |
| **Manifest Updater** | Encodes initial session state (CDN priorities, bitrate ladder info) into the `SERVER-URI` query string of each new manifest | Stateless |
| **Edge Steering Server** (this) | Per-session QoE optimization, in-stream failover, enforces master overrides. Runs at edge (Akamai, CloudFront, etc.) | **Stateless** |
| **Player** | Follows steering manifest instructions. Switches CDN pathways. Reports throughput. | Client-side |

### Why Stateless?

The edge server carries all session context in the URL parameter string. On each request:
1. The client sends the `RELOAD-URI` from the previous response (which contains encoded state)
2. The edge server decodes state, makes a decision, encodes updated state into a new `RELOAD-URI`
3. No database, no cache, no session store required

This enables deployment as a pure function on any edge compute platform at CDN scale.

---

## How It Works

### Session Lifecycle

**1. Session Start** — The manifest updater creates a new streaming manifest with a
`SERVER-URI` (HLS) or `ContentSteering` element (DASH) that includes encoded initial state:

```
# HLS Master Playlist
#EXT-X-CONTENT-STEERING:SERVER-URI="/steer?token=abc&_ss=<base64_state>",PATHWAY-ID="cdn-a"
```

```xml
<!-- DASH MPD -->
<ContentSteering defaultServiceLocation="cdn-a"
  queryBeforeStart="true">https://steer.example.com/?token=abc&_ss=<base64_state></ContentSteering>
```

**2. First Steering Request** — The player contacts the steering server:

```
GET /steer?token=abc&_ss=<base64_state>
    (DASH with queryBeforeStart: no _DASH_ params yet)

GET /steer?token=abc&_ss=<base64_state>&_HLS_pathway=cdn-a&_HLS_throughput=5140000
    (HLS: pathway and throughput from current session)
```

**3. Steering Response** — The server returns a JSON steering manifest:

```json
{
  "VERSION": 1,
  "TTL": 300,
  "RELOAD-URI": "/steer?token=abc&_ss=<updated_base64_state>",
  "PATHWAY-PRIORITY": ["cdn-a", "cdn-b"]
}
```

**4. TTL Loop** — The player waits `TTL` seconds, then repeats from step 2 using the
`RELOAD-URI` from the previous response.

**5. CDN Switch** — If the server changes the priority order (e.g., `["cdn-b", "cdn-a"]`),
the player seamlessly switches to the new top-priority CDN at the next segment boundary.

---

## Protocol Support

### HLS Content Steering (Apple spec v1.2b1)

| Element | Value |
|---------|-------|
| Manifest tag | `#EXT-X-CONTENT-STEERING:SERVER-URI="...",PATHWAY-ID="..."` |
| Query params (client to server) | `_HLS_pathway=<current_pathway>`, `_HLS_throughput=<bps>` |
| Response key for priorities | `PATHWAY-PRIORITY` (array of Pathway IDs) |
| Pathway ID charset | `[a-zA-Z0-9._-]` |

### DASH Content Steering (DASH-IF CTS 00XX v0.9.0)

| Element | Value |
|---------|-------|
| MPD element | `<ContentSteering defaultServiceLocation="..." queryBeforeStart="...">` |
| Query params (client to server) | `_DASH_pathway="<service_location>"`, `_DASH_throughput=<bps>` |
| Response key for priorities | `SERVICE-LOCATION-PRIORITY` (array of serviceLocation IDs) |
| Proxy support | `@proxyServerURL` attribute with URL-encoded forwarding |
| queryBeforeStart | If `true`, player contacts server before first segment request |

### Common Response Format (both protocols)

```json
{
  "VERSION": 1,
  "TTL": 300,
  "RELOAD-URI": "https://steer.example.com/steer?session=abc&_ss=...",
  "PATHWAY-PRIORITY": ["cdn-a", "cdn-b"],
  "SERVICE-LOCATION-PRIORITY": ["alpha", "beta"]
}
```

- `VERSION` — Must be `1`. Clients reject unrecognized versions.
- `TTL` — Seconds until client reloads. Default/recommended: 300. Can be reduced for QoE.
- `RELOAD-URI` — URI for next request. May be relative. Contains encoded session state.
- Only one of `PATHWAY-PRIORITY` or `SERVICE-LOCATION-PRIORITY` is present per response.
- Clients MUST ignore unrecognized keys (per both specs).

### Protocol Auto-Detection

The server detects the protocol from query parameters:
- `_HLS_pathway` or `_HLS_throughput` present -> HLS
- `_DASH_pathway` or `_DASH_throughput` present -> DASH
- Neither present -> falls back to path-based detection (`/steer/hls` vs `/steer/dash`)
  or a platform wrapper hint

---

## Stateless Design

### Session State Encoding

All session context is carried in the `_ss` query parameter as URL-safe base64-encoded JSON:

```
RELOAD-URI: /steer?token=abc&userId=123&_ss=eyJwcmlvcml0aWVzIj...
                    |                       |
                    |                       +-- base64(SessionState JSON)
                    +-- passthrough params (preserved across all requests)
```

### SessionState Fields

| Field | Type | Description |
|-------|------|-------------|
| `priorities` | `string[]` | Current CDN priority order |
| `throughput_map` | `(string, u64)[]` | Per-pathway throughput observations (pathway to bps) |
| `min_bitrate` | `u64` | Minimum bitrate in encoding ladder (bps). Used for QoE threshold. |
| `max_bitrate` | `u64` | Maximum bitrate in encoding ladder (bps) |
| `duration` | `u64` | Media duration in seconds (0 = live/unknown) |
| `position` | `u64` | Approximate playback position in seconds |
| `timestamp` | `u64` | Epoch seconds when state was created |
| `override_gen` | `u64` | Last applied override generation (prevents re-applying stale overrides) |

### State Size

Typical encoded state is ~100-200 bytes for a 2-3 CDN setup. This fits comfortably within
URL length limits (2048 chars for most CDNs, 8KB for modern browsers).

---

## CDN Token Passthrough

All query parameters that are **not** `_HLS_*`, `_DASH_*`, or `_ss` are automatically
preserved in every `RELOAD-URI`. This is critical for CDN authentication.

### Example: Akamai EdgeAuth Tokens

Observed on Fandango at Home (`akacldash.vudu.com`):

```
# Initial manifest SERVER-URI includes CDN tokens:
/steer?start=1772770805&end=1772857805&userId=93334984&hashParam=a7614ed1...

# Every RELOAD-URI preserves them:
/steer?start=1772770805&end=1772857805&userId=93334984&hashParam=a7614ed1...&_ss=<state>

# Player adds protocol params on top:
/steer?start=...&end=...&userId=...&hashParam=...&_ss=<state>&_HLS_pathway=CDN-A&_HLS_throughput=5000000
```

### Supported Token Schemes

Any query-parameter-based token scheme works without modification:
- **Akamai EdgeAuth**: `start`, `end`, `userId`, `hashParam` (or `__token__` format)
- **CloudFront Signed URLs**: `Policy`, `Signature`, `Key-Pair-Id`
- **Custom HMAC**: Any `token=...` or `auth=...` parameter
- **Session IDs**: `session=...`, `sid=...`

---

## API Reference

### GET /steer[/hls|/dash]

Returns a JSON steering manifest response.

**Request query parameters:**

| Parameter | Required | Description |
|-----------|----------|-------------|
| `_HLS_pathway` | No | Current HLS pathway ID (signals HLS protocol) |
| `_HLS_throughput` | No | Client-measured throughput in bps (HLS) |
| `_DASH_pathway` | No | Current DASH service location (may be double-quoted) |
| `_DASH_throughput` | No | Client-measured throughput in bps (DASH) |
| `_ss` | No | Encoded session state from previous `RELOAD-URI` |
| *(any other)* | No | Passed through to `RELOAD-URI` unchanged |

**Response (200 OK):**

```
Content-Type: application/json
Cache-Control: no-store, no-cache
Access-Control-Allow-Origin: *
```

```json
{
  "VERSION": 1,
  "TTL": 300,
  "RELOAD-URI": "/steer?token=abc&_ss=eyJ...",
  "PATHWAY-PRIORITY": ["cdn-a", "cdn-b"]
}
```

**Error (500):**

```json
{"error": "description of what went wrong"}
```

### POST /control

Applies a control command from the master steering server.

**Request body** — one of three command types:

#### set_priorities

Force a CDN priority order:

```json
{
  "type": "set_priorities",
  "region": "us-east",
  "priorities": ["cdn-b", "cdn-a"],
  "generation": 1,
  "ttl_override": 15
}
```

| Field | Type | Description |
|-------|------|-------------|
| `region` | `string?` | Optional region filter. `null` = global. |
| `priorities` | `string[]` | New priority order. |
| `generation` | `u64` | Monotonically increasing. Command rejected if `<= current`. |
| `ttl_override` | `u32?` | Override TTL for all responses. `null` = use config default. |

#### exclude_pathway

Remove a CDN from all responses (disaster recovery / maintenance):

```json
{
  "type": "exclude_pathway",
  "region": null,
  "pathway": "cdn-c",
  "generation": 2
}
```

#### clear_overrides

Revert to master-assigned defaults:

```json
{
  "type": "clear_overrides",
  "region": null,
  "generation": 3
}
```

**Response (200 OK):** Returns the updated `OverrideState` JSON.

**Response (400 Bad Request):** If command JSON is malformed or has an unknown type.

### GET /health

```json
{"status": "ok", "engine": "apex-steering"}
```

---

## WASM API Reference

The WASM module exports five functions. All platform wrappers use these.

### `handle_steering_request(request_json, overrides_json, config_json, base_path) -> string`

Main entry point. Takes a parsed steering request and returns a JSON steering response.

| Parameter | Type | Description |
|-----------|------|-------------|
| `request_json` | `string` | JSON-serialized `SteeringRequest` |
| `overrides_json` | `string` | JSON-serialized `OverrideState` (empty string = no overrides) |
| `config_json` | `string` | JSON-serialized `PolicyConfig` (empty string = defaults) |
| `base_path` | `string` | Base path for RELOAD-URI (e.g., `"/steer"`) |

**Returns:** JSON string of the `SteeringResponse`.

### `parse_request(query_string, protocol_hint) -> string`

Convenience function that parses a raw HTTP query string into a `SteeringRequest` JSON.

| Parameter | Type | Description |
|-----------|------|-------------|
| `query_string` | `string` | Raw query string (without leading `?`) |
| `protocol_hint` | `string` | `"hls"` or `"dash"` — used when no `_HLS_`/`_DASH_` params present |

**Returns:** JSON string of the `SteeringRequest`.

### `apply_control_command(overrides_json, command_json) -> string`

Applies a master server control command to the override state.

| Parameter | Type | Description |
|-----------|------|-------------|
| `overrides_json` | `string` | Current overrides (empty string = clean state) |
| `command_json` | `string` | JSON-serialized `ControlCommand` |

**Returns:** JSON string of the updated `OverrideState`.

### `encode_initial_state(state_json) -> string`

Encodes a `SessionState` into a base64 string for embedding in manifests.
Called by the master steering server (via `/encode-state`) to set initial session state.

This function performs two actions:
1. Returns the base64-encoded state string (for embedding in `SERVER-URI`)
2. **Stores the state on the edge server** as fallback for requests without `_ss`

When a client request arrives without an `_ss` parameter, `handle_steering_request` falls
back to this stored initial state instead of using empty defaults.

| Parameter | Type | Description |
|-----------|------|-------------|
| `state_json` | `string` | JSON-serialized `SessionState` |

**Returns:** URL-safe base64 string (no padding).

### `reset_initial_state()`

Clears the stored initial state set by `encode_initial_state`. After this call,
requests without `_ss` will fall back to `SessionState::default()` (empty priorities).

Used by platform wrappers for reset operations (e.g., the local dev server's `POST /reset`).

**Returns:** Nothing.

### TypeScript Declarations

Generated automatically in `pkg/apex_steering.d.ts`:

```typescript
export function handle_steering_request(
  request_json: string, overrides_json: string,
  config_json: string, base_path: string
): string;

export function parse_request(query_string: string, protocol_hint: string): string;
export function apply_control_command(overrides_json: string, command_json: string): string;
export function encode_initial_state(state_json: string): string;
export function reset_initial_state(): void;
```

---

## QoE Optimization

The policy engine performs in-session Quality of Experience optimization by monitoring
client-reported throughput.

### How It Works

1. Client reports `_HLS_throughput` or `_DASH_throughput` on each steering request
2. If throughput falls below `degradation_factor * min_bitrate`, the current pathway
   is considered **degraded**
3. The degraded pathway is **demoted** (swapped with the next pathway in the priority list)
4. TTL is reduced to `qoe_ttl` (default: 10s) for rapid re-evaluation
5. On the next request, if throughput improves on the new pathway, TTL returns to
   `default_ttl` (300s)

### Constraints

- Only the **top-priority pathway** can be demoted (prevents cascading swaps)
- Single-pathway sessions skip QoE logic (nowhere to failover)
- If `min_bitrate` is 0 (unknown encoding ladder), QoE is skipped
- QoE never triggers if the client is reporting on a non-top pathway

### Configuration

```json
{
  "default_ttl": 300,
  "qoe_ttl": 10,
  "degradation_factor": 1.2,
  "qoe_enabled": true
}
```

| Field | Default | Description |
|-------|---------|-------------|
| `default_ttl` | `300` | Normal polling interval in seconds (per spec recommendation) |
| `qoe_ttl` | `10` | Fast polling interval during active degradation |
| `degradation_factor` | `1.2` | Threshold: degraded if throughput < factor * min_bitrate |
| `qoe_enabled` | `true` | Master switch for QoE optimization |

---

## Control Plane

### Override Precedence

When a master server pushes an override, it takes effect for all subsequent steering
requests until cleared:

1. **Priority override** (`set_priorities`) replaces the session state's priorities entirely
2. **Pathway exclusion** (`exclude_pathway`) removes pathways from whatever priority
   list is active (state or override)
3. If all pathways are excluded, the original session priorities are used as fallback
4. **Clear** (`clear_overrides`) reverts to session-state priorities

### Generation Numbers

Every control command carries a monotonically increasing `generation` number:
- Commands with `generation <= current` are **silently rejected** (idempotent replay safety)
- The edge server's `OverrideState.generation` advances with each accepted command
- The session state tracks `override_gen` to avoid re-applying stale overrides across
  RELOAD-URI boundaries

### Override State Lifecycle

Override state lives **in-memory** in the edge worker instance:
- Persists for the lifetime of the worker/isolate (typically minutes to hours)
- New worker instances start with empty overrides
- Master server should periodically re-push active overrides (heartbeat pattern)
- Alternatively, load from EdgeKV/DynamoDB on worker init

---

## Deployment

### Akamai EdgeWorkers (Primary)

```
wrappers/akamai/
  main.js         onClientRequest handler
  bundle.json     EdgeWorker metadata and path matching
```

**Bundle creation:**
```bash
wasm-pack build --target bundler --release
# Copy pkg/ contents + wrappers/akamai/* into EdgeWorker bundle
# Upload via Akamai CLI or Control Center
```

**Path matching** (configured in `bundle.json`):
- `/steer/*` — steering requests
- `/control` — master server overrides
- `/health` — health check

**WASM size**: 198KB — well within EdgeWorkers' bundle limits.

### CloudFront Lambda@Edge

```
wrappers/cloudfront/
  index.js        Lambda@Edge viewer-request handler
```

Deploy as a **viewer-request** Lambda@Edge function associated with a CloudFront distribution.
The function intercepts requests matching `/steer/*` and `/control`.

### Cloudflare Workers

```
wrappers/cloudflare/
  worker.js       Workers fetch handler (ES module format)
```

Deploy using `wrangler` with the WASM module as a binding.

### Fastly Compute

```
wrappers/fastly/
  index.js        Compute fetch event handler
```

Fastly Compute runs WASM natively. Bundle with the Fastly JS SDK.

### All Platforms — Common Pattern

Every wrapper follows the same thin pattern:

```javascript
// 1. Parse query string into SteeringRequest JSON
const requestJson = parse_request(queryString, protocolHint);

// 2. Process through WASM core into SteeringResponse JSON
const responseJson = handle_steering_request(
  requestJson, overridesJson, configJson, basePath
);

// 3. Return JSON response with no-cache headers
respond(200, { 'Content-Type': 'application/json' }, responseJson);
```

---

## Build & Test

### Prerequisites

- Rust toolchain (`rustup`)
- `wasm32-unknown-unknown` target: `rustup target add wasm32-unknown-unknown`
- `wasm-pack`: `cargo install wasm-pack`
- Node.js 18+ (for local dev server and E2E tests)

### Commands

```bash
source "$HOME/.cargo/env"

# Run all 109 Rust tests (97 unit + 12 integration)
cargo test

# Build WASM package for JS bundler environments
wasm-pack build --target bundler --release
# Output: pkg/ (~198KB .wasm + JS glue + TypeScript declarations)

# Run local dev server
node scripts/server.mjs --port 3001

# Run all 98 E2E tests (starts server automatically)
./scripts/run-tests.sh

# Run everything: cargo + WASM build + E2E
./scripts/run-tests.sh --all
```

**Note:** `wasm-opt` is disabled in `Cargo.toml` metadata because the bundled version
doesn't support bulk memory operations emitted by modern Rust. Rust's own LTO (`lto = true`)
and size optimization (`opt-level = "s"`) handle binary optimization.

---

## Scripts

```
scripts/
|-- server.mjs              Local dev HTTP server (loads WASM from pkg/)
|                            Endpoints: /, /steer, /control, /health, /encode-state, /config, /reset
|-- ui.html                  Browser-based dev UI (served at / by server.mjs)
|                            Interactive testing for all endpoints: steering, control, config, encode
|-- run-tests.sh             Orchestrator: starts server, runs all E2E suites, reports results
|                            Flags: --build (rebuild WASM), --cargo (Rust tests only), --all
|-- test-hls-session.sh      27 HLS client session tests (state encoding, multi-hop, tokens)
|-- test-dash-session.sh     22 DASH client session tests (queryBeforeStart, quoted pathways)
+-- test-control-plane.sh    49 control plane + QoE tests (overrides, exclusions, degradation,
                              master override precedence across multi-hop HLS + DASH sessions)
```

---

## Project Layout

```
apex-steering/
|-- Cargo.toml                  Rust project config (cdylib + rlib targets)
|-- Cargo.lock                  Dependency lock file
|-- README.md                   Project README
|-- CLAUDE.md                   This documentation
|-- .gitignore
|
|-- src/                        Rust core library
|   |-- lib.rs                  WASM entry points (5 exported functions),
|   |                           INITIAL_STATE thread_local storage for master-set
|   |                           initial session state, re-exports for Rust consumers,
|   |                           parse_passthrough()
|   |-- types.rs                All type definitions:
|   |                             Protocol (Hls|Dash)
|   |                             SteeringRequest, SteeringResponse
|   |                             SessionState (carried in _ss param)
|   |                             ControlCommand (set_priorities|exclude_pathway|clear_overrides)
|   |                             OverrideState, PriorityOverride
|   |-- state.rs                Stateless session management:
|   |                             encode_state() / decode_state() (base64 <-> JSON)
|   |                             parse_query() (extracts HLS/DASH params + passthrough)
|   |                             build_reload_uri() (assembles RELOAD-URI with state)
|   |                             url_decode() (percent-decoding, DASH quote stripping)
|   |-- policy.rs               CDN selection policy engine:
|   |                             PolicyConfig (ttl, qoe settings)
|   |                             evaluate() -- core decision function
|   |-- response.rs             Response construction:
|   |                             build_response() -- policy + state -> SteeringResponse
|   |                             update_throughput_map() -- per-pathway throughput tracking
|   +-- control.rs              Master-to-edge override handling:
|                                 apply_command() -- processes ControlCommands
|
|-- tests/
|   +-- integration.rs          End-to-end Rust integration tests (12 tests)
|
|-- scripts/                    Local dev server, dev UI, and E2E test scripts
|   |-- server.mjs              Node.js HTTP server loading WASM from pkg/
|   |-- ui.html                 Browser-based dev UI (served at / by server.mjs)
|   |-- run-tests.sh            Test orchestrator
|   |-- test-hls-session.sh     HLS E2E tests (27 tests)
|   |-- test-dash-session.sh    DASH E2E tests (22 tests)
|   +-- test-control-plane.sh   Control plane + QoE E2E tests (49 tests)
|
|-- wrappers/                   Platform-specific JS wrappers
|   |-- akamai/
|   |   |-- main.js             EdgeWorkers onClientRequest handler
|   |   +-- bundle.json         EdgeWorker metadata
|   |-- cloudfront/
|   |   +-- index.js            Lambda@Edge viewer-request handler
|   |-- cloudflare/
|   |   +-- worker.js           Workers fetch handler (ES module)
|   +-- fastly/
|       +-- index.js            Compute fetch event handler
|
|-- docs/                       Documentation
|   |-- architecture.md         System design, diagrams, stateless model
|   |-- client-usage.md         HLS/DASH player integration examples
|   |-- control-plane.md        Master server integration, disaster recovery
|   |-- api-reference.md        HTTP API, WASM API, JSON schemas
|   |-- configuration.md        Policy config, QoE tuning
|   +-- deployment.md           Local dev server, platform deployment guides
|
+-- pkg/                        WASM build output (generated, gitignored)
    |-- apex_steering_bg.wasm   WASM binary (~198KB)
    |-- apex_steering_bg.js     JS glue code
    |-- apex_steering.js        ES module entry point
    |-- apex_steering.d.ts      TypeScript declarations
    +-- package.json            npm package metadata
```

---

## Test Coverage

**207 tests total** (109 Rust + 98 E2E) — all passing.

### Rust Unit Tests (97)

**`state.rs` — 30 tests**
- Encode/decode roundtrips: full state, default state, many pathways, special characters
- URL safety: encoded output contains no `+`, `/`, or `=`
- Error handling: invalid base64, valid base64 but invalid JSON, empty string
- Query parsing (HLS): full params, pathway-only, throughput-only
- Query parsing (DASH): quoted pathway (`%22beta%22`), unquoted, pre-start (no params)
- Token passthrough: Akamai-style (start/end/userId/hashParam), multiple custom params
- Edge cases: empty query, empty segments (`&&`), key without value, invalid throughput,
  zero throughput, u64::MAX throughput
- URL decoding: percent encoding, plus-as-space, DASH quotes, mixed hex case, truncated %
- RELOAD-URI: with/without passthrough, state decodability, absolute base URLs, Akamai tokens

**`policy.rs` — 28 tests**
- Basic: default priorities, single pathway, empty priorities
- Format: HLS uses PATHWAY-PRIORITY, DASH uses SERVICE-LOCATION-PRIORITY
- Master overrides: replaces priorities, stale override rejected, equal generation applied,
  TTL override used/absent
- Master precedence: override replaces client state priorities, persists when client state
  has equal override_gen, newer override replaces stale client state, works for DASH protocol
- Exclusions: single, multiple, all (fallback), nonexistent (noop), combined with override
- QoE: demotes degraded, no action when OK, exactly at threshold, just below threshold,
  disabled, min_bitrate=0, non-top pathway, single pathway, unknown pathway,
  custom degradation factor, custom TTL
- Config: custom default TTL

**`response.rs` — 20 tests**
- HLS/DASH response building
- State carried through RELOAD-URI: throughput map (update existing, add new),
  position advancement (by TTL, saturation at u64::MAX), override generation tracking,
  priorities match response
- Master override persistence: override priorities persisted in RELOAD-URI state,
  newer override updates both priorities and override_gen in state
- Passthrough: Akamai tokens preserved, no throughput means no map update
- JSON format: HLS has PATHWAY-PRIORITY not SERVICE-LOCATION-PRIORITY, and vice versa

**`control.rs` — 19 tests**
- SetPriorities: clean state, without TTL, replaces existing, with region
- Stale rejection: set_priorities, exclude, clear, equal generation
- ExcludePathway: single, multiple sequential, duplicate not added twice
- ClearOverrides: resets everything, on empty state
- Sequencing: set then exclude then clear
- JSON deserialization: all three command types

### Rust Integration Tests (12)

| Test | Description |
|------|-------------|
| `hls_full_session_lifecycle` | 3-request HLS session: initial, pathway+throughput, state accumulation |
| `dash_full_session_lifecycle` | 2-request DASH session with queryBeforeStart pattern |
| `qoe_triggered_cdn_switch` | Good throughput, degraded (CDN switch, TTL=10), recovered (TTL=300) |
| `master_override_during_session` | Active session interrupted by master `set_priorities` |
| `master_override_persists_across_multi_hop` | 4-request session: override applied, persists across hops, new override replaces old |
| `master_override_applied_when_client_state_has_equal_override_gen` | Override with same generation as client state still applies (>= check) |
| `disaster_recovery_exclude_cdn` | Exclude CDN during outage, clear when recovered |
| `akamai_token_passthrough_full_session` | 3 requests verifying all 4 Akamai tokens persist |
| `dash_with_steering_token_and_session` | DASH-IF Annex A example with token forwarding |
| `initial_state_encoding_for_manifest_updater` | Manifest updater encodes state, usable in steering |
| `control_command_json_roundtrip` | JSON serialize, apply, serialize, deserialize |
| `concurrent_viewers_independent_state` | Two viewers with different CDN assignments verified independent |

### E2E HTTP Tests (98)

| Suite | Tests | What It Validates |
|-------|-------|-------------------|
| `test-hls-session.sh` | 27 | HLS session lifecycle, state encoding, Akamai token passthrough, protocol auto-detection, JSON format |
| `test-dash-session.sh` | 22 | DASH queryBeforeStart, quoted pathways, SERVICE-LOCATION-PRIORITY, token passthrough |
| `test-control-plane.sh` | 49 | set/exclude/clear commands, stale rejection, QoE demotion + recovery + edge cases, master+QoE interaction, disaster recovery, master override precedence across multi-hop HLS + DASH sessions |

---

## Reference Specifications

| Document | Version | Key Sections |
|----------|---------|-------------|
| HLS Content Steering Specification | v1.2b1 (2021-04-12) | SERVER-URI, PATHWAY-ID, Steering Manifest, Client Behavior |
| DASH-IF Content Steering | v0.9.0 (2022-07-10) | ContentSteering element (Clause 5), DCSM JSON (Clause 6), Client behavior (Clause 7), URL params (Clause 8) |
| Implementing HLS/DASH Content Steering at Scale | IBC2023 | Figure 3 (edge architecture), stateless design, distributed decision logic |

### Player Support

Content Steering is supported by:
- **AVPlayer** (iOS/tvOS/macOS) — native HLS support
- **HLS.js** — open-source HLS player
- **DASH.js** — DASH-IF reference player
- **Video.js** — via HLS.js/DASH.js plugins
- **Shaka Player** — Google's open-source player
