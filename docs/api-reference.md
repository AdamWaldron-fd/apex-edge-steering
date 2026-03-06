# API Reference

Complete reference for the HTTP endpoints and WASM module exports.

---

## Table of Contents

1. [HTTP API](#http-api)
   - [GET /steer](#get-steer)
   - [POST /control](#post-control)
   - [GET /health](#get-health)
2. [WASM API](#wasm-api)
   - [handle_steering_request](#handle_steering_request)
   - [parse_request](#parse_request)
   - [apply_control_command](#apply_control_command)
   - [encode_initial_state](#encode_initial_state)
   - [reset_initial_state](#reset_initial_state)
3. [TypeScript Declarations](#typescript-declarations)
4. [JSON Schemas](#json-schemas)

---

## HTTP API

### GET /steer

Returns a JSON steering manifest response. This is the endpoint players poll on a TTL interval.

**Path variants:** `/steer`, `/steer/hls`, `/steer/dash`

**Request:**

```
GET /steer?_ss=<state>&_HLS_pathway=cdn-a&_HLS_throughput=5140000&session=abc HTTP/1.1
```

**Query Parameters:**

| Parameter | Required | Description |
|-----------|----------|-------------|
| `_HLS_pathway` | No | Current HLS pathway ID. Presence signals HLS protocol. |
| `_HLS_throughput` | No | Client-measured throughput in bits/sec (HLS). |
| `_DASH_pathway` | No | Current DASH service location. May be double-quoted (`%22`). |
| `_DASH_throughput` | No | Client-measured throughput in bits/sec (DASH). |
| `_ss` | No | URL-safe base64-encoded session state from previous `RELOAD-URI`. |
| *(any other)* | No | Passed through unchanged to `RELOAD-URI` in the response. |

**Protocol Detection:**
- `_HLS_pathway` or `_HLS_throughput` present --> HLS
- `_DASH_pathway` or `_DASH_throughput` present --> DASH
- Neither present --> path-based (`/steer/hls` vs `/steer/dash`) or wrapper hint

**Response (200 OK):**

```
Content-Type: application/json
Cache-Control: no-store, no-cache
Access-Control-Allow-Origin: *
```

**HLS Response Body:**

```json
{
  "VERSION": 1,
  "TTL": 300,
  "RELOAD-URI": "/steer?session=abc&_ss=eyJwcml...",
  "PATHWAY-PRIORITY": ["cdn-a", "cdn-b"]
}
```

**DASH Response Body:**

```json
{
  "VERSION": 1,
  "TTL": 300,
  "RELOAD-URI": "/steer?token=xyz&_ss=eyJwcml...",
  "SERVICE-LOCATION-PRIORITY": ["alpha", "beta"]
}
```

| Field | Type | Always Present | Description |
|-------|------|----------------|-------------|
| `VERSION` | `u32` | Yes | Must be `1`. Clients reject unrecognized versions. |
| `TTL` | `u32` | Yes | Seconds until client should re-request. Default: 300. |
| `RELOAD-URI` | `string` | Yes | URI for next request. Contains encoded session state. |
| `PATHWAY-PRIORITY` | `string[]` | HLS only | Ordered pathway preference list. |
| `SERVICE-LOCATION-PRIORITY` | `string[]` | DASH only | Ordered service location preference list. |

Only one of `PATHWAY-PRIORITY` or `SERVICE-LOCATION-PRIORITY` is present per response. Clients must ignore unrecognized keys (per both specs).

**Error (500):**

```json
{"error": "description of what went wrong"}
```

---

### POST /control

Applies a control command from the master steering server. See the [Control Plane Guide](control-plane.md) for detailed usage.

**Request:**

```
POST /control HTTP/1.1
Content-Type: application/json
```

**Request Body** -- one of three command types:

**set_priorities:**

```json
{
  "type": "set_priorities",
  "region": "us-east",
  "priorities": ["cdn-b", "cdn-a"],
  "generation": 1,
  "ttl_override": 15
}
```

**exclude_pathway:**

```json
{
  "type": "exclude_pathway",
  "region": null,
  "pathway": "cdn-c",
  "generation": 2
}
```

**clear_overrides:**

```json
{
  "type": "clear_overrides",
  "region": null,
  "generation": 3
}
```

**Response (200 OK):**

Returns the updated `OverrideState`:

```json
{
  "priority_override": {
    "priorities": ["cdn-b", "cdn-a"],
    "generation": 1,
    "ttl_override": 15
  },
  "excluded_pathways": [],
  "generation": 1
}
```

**Error (400 Bad Request):**

```json
{"error": "bad command: unknown variant `invalid_type`"}
```

---

### GET /health

Simple health check endpoint.

**Response (200 OK):**

```json
{"status": "ok", "engine": "apex-edge-steering"}
```

---

## WASM API

The WASM module exports five functions via `wasm-bindgen`. All platform wrappers use these functions -- the JS layer is intentionally thin.

### handle_steering_request

Main entry point. Takes a parsed steering request and returns a JSON steering response.

```
handle_steering_request(
  request_json: string,
  overrides_json: string,
  config_json: string,
  base_path: string
) -> string
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `request_json` | `string` | JSON-serialized `SteeringRequest` |
| `overrides_json` | `string` | JSON-serialized `OverrideState`. Empty string = no overrides. |
| `config_json` | `string` | JSON-serialized `PolicyConfig`. Empty string = defaults. |
| `base_path` | `string` | Base path for RELOAD-URI (e.g., `"/steer"`). |

**Returns:** JSON string of `SteeringResponse`.

**Throws:** `JsError` if request JSON is malformed or processing fails.

**Example:**

```javascript
const requestJson = JSON.stringify({
  protocol: "hls",
  pathway: "cdn-a",
  throughput: 5140000,
  session_state: { priorities: ["cdn-a", "cdn-b"], min_bitrate: 783322, max_bitrate: 4530860 },
  raw_query: "session=abc&_HLS_pathway=cdn-a&_HLS_throughput=5140000"
});

const response = handle_steering_request(requestJson, '', '', '/steer');
console.log(JSON.parse(response));
// { VERSION: 1, TTL: 300, "RELOAD-URI": "...", "PATHWAY-PRIORITY": ["cdn-a", "cdn-b"] }
```

---

### parse_request

Convenience function that parses a raw HTTP query string into a `SteeringRequest` JSON. Useful for platform wrappers that receive raw query strings.

```
parse_request(query_string: string, protocol_hint: string) -> string
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `query_string` | `string` | Raw query string without leading `?`. |
| `protocol_hint` | `string` | `"hls"` or `"dash"`. Used when no `_HLS_`/`_DASH_` params present. |

**Returns:** JSON string of `SteeringRequest`.

**Example:**

```javascript
const requestJson = parse_request(
  'session=abc&_ss=eyJ...&_HLS_pathway=cdn-a&_HLS_throughput=5140000',
  'hls'
);
// Returns: {"protocol":"hls","pathway":"cdn-a","throughput":5140000,"session_state":{...},"raw_query":"..."}
```

---

### apply_control_command

Applies a master server control command to the override state.

```
apply_control_command(overrides_json: string, command_json: string) -> string
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `overrides_json` | `string` | Current overrides JSON. Empty string = clean state. |
| `command_json` | `string` | JSON-serialized `ControlCommand`. |

**Returns:** JSON string of updated `OverrideState`.

**Example:**

```javascript
let overrides = '';

// Apply set_priorities
overrides = apply_control_command(overrides, JSON.stringify({
  type: 'set_priorities',
  region: null,
  priorities: ['cdn-b', 'cdn-a'],
  generation: 1,
  ttl_override: 30,
}));

console.log(JSON.parse(overrides));
// { priority_override: { priorities: ["cdn-b","cdn-a"], generation: 1, ttl_override: 30 },
//   excluded_pathways: [], generation: 1 }

// Apply exclude_pathway
overrides = apply_control_command(overrides, JSON.stringify({
  type: 'exclude_pathway',
  region: null,
  pathway: 'cdn-a',
  generation: 2,
}));

// Apply clear
overrides = apply_control_command(overrides, JSON.stringify({
  type: 'clear_overrides',
  region: null,
  generation: 3,
}));
```

---

### encode_initial_state

Encodes a `SessionState` into a URL-safe base64 string for embedding in manifests. Called by the master steering server to set initial session state on the edge server.

This function performs two actions:
1. Returns the base64-encoded state string (for embedding in `SERVER-URI`)
2. **Stores the state on the edge server** as fallback for client requests without `_ss`

When `handle_steering_request` receives a request without an `_ss` parameter, it falls back to
this stored initial state instead of using empty defaults. This ensures the first client request
returns correct priorities even before the client has received a `RELOAD-URI`.

```
encode_initial_state(state_json: string) -> string
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `state_json` | `string` | JSON-serialized `SessionState`. |

**Returns:** URL-safe base64 string (no padding).

**Example:**

```javascript
const encoded = encode_initial_state(JSON.stringify({
  priorities: ['cdn-a', 'cdn-b'],
  throughput_map: [],
  min_bitrate: 783322,
  max_bitrate: 4530860,
  duration: 3600,
  position: 0,
  timestamp: 1700000000,
  override_gen: 0,
}));

// The state is now stored on the edge server.
// Subsequent requests to /steer without _ss will use these priorities.

// Use in HLS manifest:
// #EXT-X-CONTENT-STEERING:SERVER-URI="/steer?_ss=${encoded}",PATHWAY-ID="cdn-a"
```

---

### reset_initial_state

Clears the stored initial state set by `encode_initial_state`. After this call,
requests without `_ss` will fall back to `SessionState::default()` (empty priorities).

Used by platform wrappers for reset operations (e.g., the local dev server's `POST /reset`).

```
reset_initial_state()
```

**Returns:** Nothing.

**Example:**

```javascript
// Clear stored initial state
reset_initial_state();

// Now requests without _ss will return empty priorities
```

---

## TypeScript Declarations

Generated automatically in `pkg/apex_edge_steering.d.ts` by `wasm-pack build`:

```typescript
/**
 * Process a steering request and return a JSON steering response.
 */
export function handle_steering_request(
  request_json: string,
  overrides_json: string,
  config_json: string,
  base_path: string,
): string;

/**
 * Parse a raw query string into a SteeringRequest JSON.
 */
export function parse_request(
  query_string: string,
  protocol_hint: string,
): string;

/**
 * Apply a control command. Returns updated overrides JSON.
 */
export function apply_control_command(
  overrides_json: string,
  command_json: string,
): string;

/**
 * Encode a SessionState into a base64 string for manifests.
 * Also stores the state on the edge server as fallback for requests without _ss.
 */
export function encode_initial_state(state_json: string): string;

/**
 * Clear the stored initial state.
 */
export function reset_initial_state(): void;
```

---

## JSON Schemas

### SteeringRequest

```json
{
  "protocol": "hls" | "dash",
  "pathway": "cdn-a" | null,
  "throughput": 5140000 | null,
  "session_state": SessionState | null,
  "raw_query": "session=abc&_HLS_pathway=cdn-a&..."
}
```

### SessionState

```json
{
  "priorities": ["cdn-a", "cdn-b"],
  "throughput_map": [["cdn-a", 5140000]],
  "min_bitrate": 783322,
  "max_bitrate": 4530860,
  "duration": 3600,
  "position": 120,
  "timestamp": 1700000000,
  "override_gen": 0
}
```

### SteeringResponse

```json
{
  "VERSION": 1,
  "TTL": 300,
  "RELOAD-URI": "/steer?_ss=...",
  "PATHWAY-PRIORITY": ["cdn-a", "cdn-b"],
  "SERVICE-LOCATION-PRIORITY": null
}
```

Note: Only one of `PATHWAY-PRIORITY` / `SERVICE-LOCATION-PRIORITY` is present. The absent key is omitted entirely (not `null`).

### ControlCommand (tagged union)

```json
// set_priorities
{ "type": "set_priorities", "region": string | null, "priorities": string[], "generation": u64, "ttl_override": u32 | null }

// exclude_pathway
{ "type": "exclude_pathway", "region": string | null, "pathway": string, "generation": u64 }

// clear_overrides
{ "type": "clear_overrides", "region": string | null, "generation": u64 }
```

### OverrideState

```json
{
  "priority_override": PriorityOverride | null,
  "excluded_pathways": ["cdn-c"],
  "generation": 5
}
```

### PriorityOverride

```json
{
  "priorities": ["cdn-b", "cdn-a"],
  "generation": 5,
  "ttl_override": 30
}
```

### PolicyConfig

```json
{
  "default_ttl": 300,
  "qoe_ttl": 10,
  "degradation_factor": 1.2,
  "qoe_enabled": true
}
```
