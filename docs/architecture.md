# Architecture & Design

## Overview

apex-steering is a **stateless edge content steering server** that makes per-session CDN routing decisions at the edge. It implements the two-stage architecture from [Implementing HLS/DASH Content Steering at Scale](https://www.ibc.org/technical-papers/implementing-hls-dash-content-steering-at-scale/10567.article) (Reznik et al., Brightcove, IBC2023, Figure 3).

---

## System Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         STEERING MASTER (Stateful)                         │
│                                                                             │
│  Responsibilities:                                                          │
│  - Global / regional CDN load balancing                                     │
│  - COGS optimization (cost-based CDN selection)                             │
│  - Contract commit management (traffic volume guarantees)                   │
│  - Disaster recovery initiation                                             │
│                                                                             │
│  Outputs:                                                                   │
│  1. Initial CDN priority order per new session (via Manifest Updater)       │
│  2. Runtime overrides via POST /control to edge servers                     │
└──────────┬──────────────────────────────────────────────┬───────────────────┘
           │                                              │
           │ Initial CDN order                   POST /control
           │ (embedded in manifests)             (set_priorities,
           │                                      exclude_pathway,
           │                                      clear_overrides)
           │                                              │
           v                                              v
┌──────────────────┐                           ┌──────────────────────────────┐
│  MANIFEST        │                           │  EDGE STEERING SERVER        │
│  UPDATER         │                           │  (apex-steering)             │
│                  │                           │                              │
│  Encodes initial │                           │  ┌────────────────────────┐  │
│  SessionState    │                           │  │   Rust -> WASM Core    │  │
│  into SERVER-URI │                           │  │                        │  │
│  of each new     │                           │  │  state.rs  - decode/   │  │
│  manifest        │                           │  │             encode _ss │  │
│                  │                           │  │  policy.rs - QoE +     │  │
│  [Stateless]     │                           │  │             overrides  │  │
│                  │                           │  │  control.rs- master    │  │
│                  │                           │  │             commands   │  │
│                  │                           │  └────────────────────────┘  │
└────────┬─────────┘                           │                              │
         │                                     │  Runs on:                    │
         │ embeds                               │  - Akamai EdgeWorkers       │
         │ #EXT-X-CONTENT-STEERING:SERVER-URI   │  - CloudFront Lambda@Edge   │
         │    or                                │  - Cloudflare Workers       │
         │ <ContentSteering>                    │  - Fastly Compute           │
         │                                     │                              │
         v                                     │  [Stateless - all context    │
┌──────────────────┐                           │   in URL params]             │
│  CDN EDGE        │                           └──────────────▲───────────────┘
│  (Akamai, CF,    │                                          │
│   CloudFront...) │                             TTL-based polling
│                  │                             + _HLS_pathway
│  Serves HLS/DASH │                             + _HLS_throughput
│  manifests &     │                             + _DASH_pathway
│  segments        │                             + _DASH_throughput
│                  │                                          │
└────────┬─────────┘                           ┌──────────────┴───────────────┐
         │                                     │  PLAYER                      │
         │  manifests + segments               │                              │
         └────────────────────────────────────>│  AVPlayer (iOS/tvOS/macOS)   │
                                               │  HLS.js                      │
                                               │  DASH.js                     │
                                               │  Shaka Player                │
                                               │  Video.js (via plugins)      │
                                               └──────────────────────────────┘
```

---

## Component Responsibilities

| Component | Role | State | Where |
|-----------|------|-------|-------|
| **Steering Master** | Global CDN decisions: load balancing, COGS, contracts, DR | Stateful | Central/cloud |
| **Manifest Updater** | Encodes initial session state into manifest `SERVER-URI` | Stateless | Origin-side |
| **Edge Steering Server** | Per-session QoE, failover, enforces master overrides | **Stateless** | CDN edge |
| **Player** | Follows steering instructions, reports throughput | Client | End-user device |

---

## Stateless Design

### The Problem

Traditional steering servers require server-side session storage (Redis, DynamoDB, etc.) to track each viewer's state. At CDN scale with millions of concurrent sessions, this introduces:
- Latency from database reads on every TTL-interval request
- Cost of a distributed session store
- Complexity of cache invalidation and replication
- Single points of failure

### The Solution

All session context is encoded into the `RELOAD-URI` returned to the player. On each request, the server:

1. **Decodes** session state from the `_ss` query parameter
2. **Evaluates** the policy (overrides + QoE)
3. **Encodes** updated state into a new `_ss` parameter in the response `RELOAD-URI`

```
                    ┌─────────────────────────────────────────────────┐
                    │                  RELOAD-URI                      │
                    │                                                  │
                    │  /steer?token=abc&userId=123&_ss=eyJwcm...       │
                    │         ├───────────────────┤    ├──────────┤    │
                    │         │                   │    │          │    │
                    │         │  Passthrough       │    │  Session  │    │
                    │         │  params (CDN       │    │  State    │    │
                    │         │  tokens, etc.)     │    │  (base64) │    │
                    │         │                   │    │          │    │
                    │         │  Preserved across  │    │  Updated  │    │
                    │         │  all requests      │    │  each     │    │
                    │         │                   │    │  response  │    │
                    │         └───────────────────┘    └──────────┘    │
                    └─────────────────────────────────────────────────┘
```

### Session State Fields

The `_ss` parameter contains URL-safe base64-encoded JSON:

```json
{
  "priorities": ["cdn-a", "cdn-b", "cdn-c"],
  "throughput_map": [["cdn-a", 5140000], ["cdn-b", 3200000]],
  "min_bitrate": 783322,
  "max_bitrate": 4530860,
  "duration": 3600,
  "position": 120,
  "timestamp": 1700000000,
  "override_gen": 0
}
```

| Field | Type | Description |
|-------|------|-------------|
| `priorities` | `string[]` | Current CDN priority order |
| `throughput_map` | `[string, u64][]` | Per-pathway throughput observations (bps) |
| `min_bitrate` | `u64` | Minimum bitrate in encoding ladder (bps) -- used for QoE threshold |
| `max_bitrate` | `u64` | Maximum bitrate in encoding ladder (bps) |
| `duration` | `u64` | Media duration in seconds (0 = live/unknown) |
| `position` | `u64` | Approximate playback position in seconds |
| `timestamp` | `u64` | Epoch seconds when state was first created |
| `override_gen` | `u64` | Last applied override generation (prevents re-applying stale overrides) |

**Typical encoded size:** ~100-200 bytes for a 2-3 CDN setup, well within URL length limits.

---

## Data Flow: Complete Session Lifecycle

```
 Time   Player                  Edge Server              Master
  │
  │     ┌───────────────────────────────────────────────────────────────┐
  │     │ SESSION START: Manifest updater embeds SERVER-URI with       │
  │     │ initial state (_ss) into HLS/DASH manifest                   │
  │     └───────────────────────────────────────────────────────────────┘
  │
  t=0   GET /steer?_ss=<init>
  │     ─────────────────────>
  │                            decode _ss
  │                            no overrides, no QoE data
  │                            use initial priorities
  │                            encode updated _ss (position += TTL)
  │     <─────────────────────
  │     { VERSION:1, TTL:300,
  │       RELOAD-URI: "..._ss=<s1>",
  │       PATHWAY-PRIORITY: ["cdn-a","cdn-b"] }
  │
  │     (player uses cdn-a for 300 seconds)
  │
  t=300 GET /steer?_ss=<s1>
  │     &_HLS_pathway=cdn-a
  │     &_HLS_throughput=5140000
  │     ─────────────────────>
  │                            decode _ss=<s1>
  │                            throughput OK (5.1M > 1.2 * min_bitrate)
  │                            keep cdn-a on top
  │                            record throughput in state
  │     <─────────────────────
  │     { TTL:300,
  │       PATHWAY-PRIORITY: ["cdn-a","cdn-b"] }
  │
  │                                                      ┌──────────────┐
  │                            POST /control             │ CDN-A outage │
  │                            {"type":"exclude_pathway", │ detected     │
  │                             "pathway":"cdn-a",       └──────────────┘
  │                             "generation":1}
  │                            <──────────────────────────
  │                            overrides updated
  │
  t=600 GET /steer?_ss=<s2>
  │     &_HLS_pathway=cdn-a
  │     &_HLS_throughput=50000    (degraded!)
  │     ─────────────────────>
  │                            cdn-a excluded by master
  │                            QoE also detects degradation
  │                            promote cdn-b
  │     <─────────────────────
  │     { TTL:10,              <── fast poll during incident
  │       PATHWAY-PRIORITY: ["cdn-b"] }
  │
  │     (player switches to cdn-b)
  │
  t=610 GET /steer?_ss=<s3>
  │     &_HLS_pathway=cdn-b
  │     &_HLS_throughput=6000000
  │     ─────────────────────>
  │                            cdn-b healthy
  │                            cdn-a still excluded
  │     <─────────────────────
  │     { TTL:300,
  │       PATHWAY-PRIORITY: ["cdn-b"] }
```

---

## CDN Token Passthrough

All query parameters that are **not** `_HLS_*`, `_DASH_*`, or `_ss` are automatically preserved in every `RELOAD-URI`. This is critical for CDN authentication.

```
Request flow with Akamai EdgeAuth tokens:

  Initial SERVER-URI:
    /steer?start=1772770805&end=1772857805&userId=93334984&hashParam=a7614ed1

  Every RELOAD-URI preserves them:
    /steer?start=1772770805&end=1772857805&userId=93334984&hashParam=a7614ed1&_ss=<state>

  Player appends protocol params:
    /steer?start=...&end=...&userId=...&hashParam=...&_ss=...&_HLS_pathway=cdn-a&_HLS_throughput=5000000
```

**Supported token schemes** (any query-parameter-based scheme works):
- Akamai EdgeAuth: `start`, `end`, `userId`, `hashParam`
- CloudFront Signed URLs: `Policy`, `Signature`, `Key-Pair-Id`
- Custom HMAC: any `token=...` or `auth=...` parameter
- Session IDs: `session=...`, `sid=...`

---

## QoE Decision Flow

```
                         ┌──────────────────┐
                         │ Client reports   │
                         │ throughput       │
                         └────────┬─────────┘
                                  │
                                  v
                    ┌─────────────────────────────┐
                    │ QoE enabled in config?       │── No ──> Keep current priorities
                    └─────────────┬───────────────┘          TTL = default_ttl
                                  │ Yes
                                  v
                    ┌─────────────────────────────┐
                    │ min_bitrate > 0?             │── No ──> Keep current priorities
                    │ (encoding ladder known)      │          (can't determine threshold)
                    └─────────────┬───────────────┘
                                  │ Yes
                                  v
                    ┌─────────────────────────────┐
                    │ throughput <                 │── No ──> Keep current priorities
                    │   degradation_factor *       │          TTL = default_ttl
                    │   min_bitrate?               │
                    └─────────────┬───────────────┘
                                  │ Yes (degraded)
                                  v
                    ┌─────────────────────────────┐
                    │ Is degraded pathway the      │── No ──> Keep current priorities
                    │ TOP priority? (position 0)   │          (only top can be demoted)
                    └─────────────┬───────────────┘
                                  │ Yes
                                  v
                    ┌─────────────────────────────┐
                    │ More than 1 pathway?         │── No ──> Keep current priorities
                    │ (need somewhere to failover) │          (single CDN, nowhere to go)
                    └─────────────┬───────────────┘
                                  │ Yes
                                  v
                    ┌─────────────────────────────┐
                    │ DEMOTE: swap position 0 ↔ 1 │
                    │ SET TTL = qoe_ttl (10s)     │
                    │ (fast re-evaluation cycle)   │
                    └─────────────────────────────┘
```

---

## Override Precedence

When processing a steering request, priorities are determined in this order:

```
  1. Start with session state priorities
         │
         v
  2. Apply master priority override (set_priorities)?
     ── Only if override.generation >= session.override_gen
         │
         v
  3. Remove excluded pathways (exclude_pathway)?
     ── Filter from whatever priority list is active
         │
         v
  4. All pathways excluded?
     ── Yes: fall back to original session state priorities
     ── No: continue
         │
         v
  5. Determine TTL
     ── override.ttl_override if present
     ── else config.default_ttl
         │
         v
  6. Apply QoE optimization?
     ── May swap top two priorities and reduce TTL
         │
         v
  7. Return SteeringResponse
```

---

## Generation Numbers

Generation numbers provide **idempotent, replay-safe** command processing:

```
  Master sends:  gen=1  gen=2  gen=3  gen=2 (replay)  gen=4
                   │      │      │      │                │
  Edge state:    gen=0  gen=1  gen=2   gen=3            gen=3
                   │      │      │      │                │
  Action:       APPLY  APPLY  APPLY  REJECT            APPLY
                                     (stale)
```

- Control commands use **strictly greater than** current generation (rejected if `<=`)
- Session state tracks `override_gen` to prevent re-applying stale overrides across RELOAD-URI boundaries
- Master should use monotonically increasing values (Unix timestamp works well)

---

## WASM Module Design

```
┌─────────────────────────────────────────────────────────┐
│                    WASM Core (198 KB)                     │
│                                                          │
│  Exports (via wasm-bindgen):                             │
│                                                          │
│  ┌──────────────────────────────────────────────────┐   │
│  │ handle_steering_request(req, ovr, cfg, path)     │   │
│  │   -> SteeringResponse JSON                       │   │
│  │   Main entry point for all steering requests      │   │
│  │   Falls back to stored initial state when _ss     │   │
│  │   is absent from the client request               │   │
│  └──────────────────────────────────────────────────┘   │
│                                                          │
│  ┌──────────────────────────────────────────────────┐   │
│  │ parse_request(query_string, protocol_hint)       │   │
│  │   -> SteeringRequest JSON                        │   │
│  │   Convenience: raw query string -> parsed request │   │
│  └──────────────────────────────────────────────────┘   │
│                                                          │
│  ┌──────────────────────────────────────────────────┐   │
│  │ apply_control_command(overrides, command)         │   │
│  │   -> OverrideState JSON                          │   │
│  │   Process master commands                         │   │
│  └──────────────────────────────────────────────────┘   │
│                                                          │
│  ┌──────────────────────────────────────────────────┐   │
│  │ encode_initial_state(state)                      │   │
│  │   -> base64 string                               │   │
│  │   Stores state on edge server + returns encoded   │   │
│  │   string for manifest updater SERVER-URI          │   │
│  └──────────────────────────────────────────────────┘   │
│                                                          │
│  ┌──────────────────────────────────────────────────┐   │
│  │ reset_initial_state()                            │   │
│  │   Clears stored initial state (for reset ops)    │   │
│  └──────────────────────────────────────────────────┘   │
│                                                          │
│  Internal modules:                                       │
│    state.rs   -> base64 encode/decode, query parsing     │
│    policy.rs  -> CDN selection, QoE optimization         │
│    control.rs -> override command processing              │
│    response.rs -> response construction + state update    │
│    types.rs   -> all shared type definitions              │
└─────────────────────────────────────────────────────────┘
          │              │              │              │
          v              v              v              v
   ┌──────────┐   ┌──────────┐  ┌───────────┐  ┌──────────┐
   │  Akamai  │   │CloudFront│  │Cloudflare │  │  Fastly  │
   │EdgeWorker│   │Lambda@   │  │  Worker   │  │Compute@  │
   │ main.js  │   │Edge      │  │ worker.js │  │Edge      │
   │          │   │index.js  │  │           │  │index.js  │
   └──────────┘   └──────────┘  └───────────┘  └──────────┘
```

Each platform wrapper is a thin JS adapter (~80-100 lines) that:
1. Routes HTTP requests to the appropriate WASM function
2. Manages in-memory override state
3. Returns responses with correct headers

The WASM core contains **all** protocol logic, policy decisions, and state management.
