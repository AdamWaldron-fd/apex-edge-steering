# Client Usage Guide

This guide shows how to integrate apex-edge-steering with HLS and DASH players. The examples cover manifest setup, player configuration, and what happens during a streaming session.

---

## Table of Contents

1. [HLS Integration](#hls-integration)
2. [DASH Integration](#dash-integration)
3. [Initial State Setup](#initial-state-setup)
4. [Session Lifecycle Examples](#session-lifecycle-examples)
5. [CDN Token Passthrough](#cdn-token-passthrough)
6. [Player Compatibility](#player-compatibility)

---

## HLS Integration

### 1. Manifest Setup

Add the `#EXT-X-CONTENT-STEERING` tag to your multivariant (master) playlist. The manifest updater generates the `SERVER-URI` with encoded initial session state.

```m3u8
#EXTM3U
#EXT-X-CONTENT-STEERING:SERVER-URI="/steer?_ss=eyJwcmlvcml0aWVzIjpbImNkbi1hIiwiY2RuLWIiXSw...",PATHWAY-ID="cdn-a"

#EXT-X-STREAM-INF:BANDWIDTH=4530860,PATHWAY-ID="cdn-a"
https://cdn-a.example.com/video/1080p/stream.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=4530860,PATHWAY-ID="cdn-b"
https://cdn-b.example.com/video/1080p/stream.m3u8

#EXT-X-STREAM-INF:BANDWIDTH=2400000,PATHWAY-ID="cdn-a"
https://cdn-a.example.com/video/720p/stream.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2400000,PATHWAY-ID="cdn-b"
https://cdn-b.example.com/video/720p/stream.m3u8

#EXT-X-STREAM-INF:BANDWIDTH=783322,PATHWAY-ID="cdn-a"
https://cdn-a.example.com/video/480p/stream.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=783322,PATHWAY-ID="cdn-b"
https://cdn-b.example.com/video/480p/stream.m3u8
```

Key points:
- `SERVER-URI` contains the steering endpoint with encoded initial state in `_ss`
- `PATHWAY-ID` on each variant identifies which CDN serves it
- Each rendition must have variants from every CDN pathway
- The player starts with `PATHWAY-ID="cdn-a"` (the default pathway)

### 2. What the Player Sends

After loading the manifest, the player periodically polls the steering server. The player automatically appends:

```
GET /steer?_ss=<state>&_HLS_pathway=cdn-a&_HLS_throughput=5140000
```

| Parameter | Value | Description |
|-----------|-------|-------------|
| `_HLS_pathway` | `cdn-a` | The pathway the player is currently using |
| `_HLS_throughput` | `5140000` | Client-measured throughput in bits/sec |

### 3. What the Player Receives

```json
{
  "VERSION": 1,
  "TTL": 300,
  "RELOAD-URI": "/steer?_ss=eyJwcmlvcml0aWVzIj...",
  "PATHWAY-PRIORITY": ["cdn-a", "cdn-b"]
}
```

The player:
- Uses `PATHWAY-PRIORITY[0]` as the preferred CDN for segment downloads
- Waits `TTL` seconds before making the next request to `RELOAD-URI`
- Ignores any keys it doesn't recognize (per spec)

### 4. HLS.js Example

```javascript
import Hls from 'hls.js';

const hls = new Hls({
  // HLS.js supports Content Steering natively (v1.4+)
  // No special configuration needed -- it reads #EXT-X-CONTENT-STEERING
  // from the manifest and handles the steering loop automatically.
});

hls.loadSource('https://cdn.example.com/master.m3u8');
hls.attachMedia(videoElement);

// Monitor steering events
hls.on(Hls.Events.STEERING_MANIFEST_LOADED, (event, data) => {
  console.log('Steering response:', data.steeringManifest);
  console.log('Current pathway:', data.pathwayId);
});
```

### 5. AVPlayer Example (Swift)

```swift
// AVPlayer handles Content Steering natively on iOS 17+ / tvOS 17+ / macOS 14+
// No code changes required -- just ensure the manifest has #EXT-X-CONTENT-STEERING

let url = URL(string: "https://cdn.example.com/master.m3u8")!
let playerItem = AVPlayerItem(url: url)
let player = AVPlayer(playerItem: playerItem)

// Monitor pathway switches via KVO
playerItem.addObserver(self, forKeyPath: "currentMediaSelection", options: [.new], context: nil)
```

---

## DASH Integration

### 1. MPD Setup

Add the `<ContentSteering>` element to your DASH MPD:

```xml
<?xml version="1.0" encoding="utf-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     profiles="urn:mpeg:dash:profile:isoff-live:2011"
     type="static"
     mediaPresentationDuration="PT1H">

  <ContentSteering
    defaultServiceLocation="alpha"
    queryBeforeStart="true">
      https://steer.example.com/steer?token=234523452&amp;_ss=eyJwcmlvcml0aWVzIj...
  </ContentSteering>

  <Period>
    <AdaptationSet mimeType="video/mp4">
      <!-- Representations from CDN "alpha" -->
      <Representation id="v1" bandwidth="4530860"
        serviceLocation="alpha">
        <BaseURL>https://alpha.cdn.example.com/video/</BaseURL>
      </Representation>
      <!-- Same representation from CDN "beta" -->
      <Representation id="v1" bandwidth="4530860"
        serviceLocation="beta">
        <BaseURL>https://beta.cdn.example.com/video/</BaseURL>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>
```

Key points:
- `defaultServiceLocation="alpha"` -- initial CDN preference
- `queryBeforeStart="true"` -- player contacts steering server before first segment (recommended)
- `serviceLocation` on each `Representation` maps to a CDN
- DASH uses `SERVICE-LOCATION-PRIORITY` instead of `PATHWAY-PRIORITY`

### 2. What the Player Sends

**First request** (with `queryBeforeStart="true"`, no pathway yet):

```
GET /steer?token=234523452&_ss=<state>
```

**Subsequent requests** (player appends pathway and throughput):

```
GET /steer?token=234523452&_ss=<state>&_DASH_pathway=%22alpha%22&_DASH_throughput=5140000
```

Note: DASH spec requires `_DASH_pathway` values to be double-quoted (URL-encoded as `%22`). apex-edge-steering handles both quoted and unquoted values.

### 3. What the Player Receives

```json
{
  "VERSION": 1,
  "TTL": 300,
  "RELOAD-URI": "/steer?token=234523452&_ss=eyJwcmlvcml0aWVzIj...",
  "SERVICE-LOCATION-PRIORITY": ["alpha", "beta"]
}
```

### 4. DASH.js Example

```javascript
import dashjs from 'dashjs';

const player = dashjs.MediaPlayer().create();

// DASH.js supports Content Steering natively (v4.7+)
// It reads <ContentSteering> from the MPD automatically.
player.initialize(videoElement, 'https://cdn.example.com/manifest.mpd', true);

// Monitor steering events
player.on(dashjs.MediaPlayer.events.CONTENT_STEERING_REQUEST_COMPLETED, (e) => {
  console.log('Steering response received');
  console.log('Service locations:', e.currentSteeringResponseData);
});
```

### 5. Shaka Player Example

```javascript
import shaka from 'shaka-player';

const player = new shaka.Player(videoElement);

// Shaka Player supports Content Steering natively (v4.3+)
player.load('https://cdn.example.com/manifest.mpd');
```

---

## Initial State Setup

The manifest updater (or master steering server) needs to encode initial session state into the `SERVER-URI` or `<ContentSteering>` URL. Use the `encode_initial_state` WASM function, which both returns the encoded string and stores the state on the edge server as fallback for requests without `_ss`:

### Node.js Example (Manifest Updater)

```javascript
const { encode_initial_state } = require('apex-edge-steering');

function generateServerUri(basePath, priorities, encodingLadder, tokens) {
  // Build the initial session state
  const state = JSON.stringify({
    priorities: priorities,          // e.g., ["cdn-a", "cdn-b"]
    throughput_map: [],              // empty on first request
    min_bitrate: encodingLadder.min, // e.g., 783322 (480p)
    max_bitrate: encodingLadder.max, // e.g., 4530860 (1080p)
    duration: encodingLadder.duration || 0,
    position: 0,
    timestamp: Math.floor(Date.now() / 1000),
    override_gen: 0,
  });

  // Encode to URL-safe base64
  const encoded = encode_initial_state(state);

  // Build the full SERVER-URI with any CDN tokens
  const params = new URLSearchParams(tokens);
  params.set('_ss', encoded);

  return `${basePath}?${params.toString()}`;
}

// Usage:
const serverUri = generateServerUri(
  '/steer',
  ['cdn-a', 'cdn-b'],
  { min: 783322, max: 4530860, duration: 3600 },
  { session: 'abc123', token: 'xyz' }
);
// Result: /steer?session=abc123&token=xyz&_ss=eyJwcmlvcml0aWVzIj...

// Embed in HLS manifest:
// #EXT-X-CONTENT-STEERING:SERVER-URI="/steer?session=abc123&token=xyz&_ss=...",PATHWAY-ID="cdn-a"
```

### Rust Example (Manifest Updater)

```rust
use apex_edge_steering::{encode_state, types::SessionState};

let state = SessionState {
    priorities: vec!["cdn-a".into(), "cdn-b".into()],
    min_bitrate: 783_322,
    max_bitrate: 4_530_860,
    duration: 3600,
    position: 0,
    timestamp: 1700000000,
    override_gen: 0,
    ..Default::default()
};

let encoded = encode_state(&state).unwrap();
let server_uri = format!("/steer?session=abc123&_ss={encoded}");
```

---

## Session Lifecycle Examples

### Example 1: Normal HLS Session (3 Requests)

```
                                                         Priorities
Request 1: GET /steer?_ss=<init>                        returned
  - No pathway or throughput yet (first request)         ─────────
  - Server returns default priorities                    ["cdn-a", "cdn-b"]
  - TTL = 300s                                           TTL = 300

                (player downloads from cdn-a for 300s)

Request 2: GET /steer?_ss=<s1>&_HLS_pathway=cdn-a&_HLS_throughput=5140000
  - cdn-a throughput healthy (5.1 Mbps > 1.2 * 783 Kbps)
  - No change to priorities                              ["cdn-a", "cdn-b"]
  - TTL = 300s                                           TTL = 300

                (player continues on cdn-a for 300s)

Request 3: GET /steer?_ss=<s2>&_HLS_pathway=cdn-a&_HLS_throughput=6000000
  - cdn-a still healthy
  - State accumulates: position advanced, throughput recorded
  - Session continues normally                           ["cdn-a", "cdn-b"]
                                                         TTL = 300
```

### Example 2: QoE-Triggered CDN Switch

```
Request 1: throughput = 5,000,000 bps on cdn-a
  - Healthy (5M > 1.2 * 1M threshold)                   ["cdn-a", "cdn-b"]
  - TTL = 300s                                           TTL = 300

Request 2: throughput = 500,000 bps on cdn-a
  - DEGRADED (500K < 1.2 * 1M threshold)
  - cdn-a demoted, cdn-b promoted                        ["cdn-b", "cdn-a"]
  - TTL = 10s (fast re-evaluation)                       TTL = 10

                (player switches to cdn-b)

Request 3: throughput = 6,000,000 bps on cdn-b
  - cdn-b healthy
  - TTL returns to normal                                ["cdn-b", "cdn-a"]
                                                         TTL = 300
```

### Example 3: DASH queryBeforeStart Session

```
Request 1: GET /steer?token=234523452&_ss=<init>
  - queryBeforeStart=true, no _DASH_ params yet
  - Protocol detected as DASH from hint (path or config)
  - Returns SERVICE-LOCATION-PRIORITY                    ["alpha", "beta"]
                                                         TTL = 300

                (player starts playback on "alpha")

Request 2: GET /steer?token=234523452&_ss=<s1>&_DASH_pathway=%22alpha%22&_DASH_throughput=5140000
  - Protocol auto-detected from _DASH_ params
  - Quoted pathway value decoded to "alpha"
  - Normal steering continues                            ["alpha", "beta"]
                                                         TTL = 300
```

---

## CDN Token Passthrough

apex-edge-steering automatically preserves all non-steering query parameters across every `RELOAD-URI`. This is essential for CDN authentication tokens.

### Akamai EdgeAuth Example

```
Initial manifest SERVER-URI:
  /steer?start=1772770805&end=1772857805&userId=93334984&hashParam=a7614ed1&_ss=<init>

Request 1 from player:
  /steer?start=1772770805&end=1772857805&userId=93334984&hashParam=a7614ed1&_ss=<init>&_HLS_pathway=cdn-a&_HLS_throughput=5000000

Response RELOAD-URI:
  /steer?start=1772770805&end=1772857805&userId=93334984&hashParam=a7614ed1&_ss=<s1>
         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
         All 4 Akamai tokens preserved

Request 2 from player:
  /steer?start=1772770805&end=1772857805&userId=93334984&hashParam=a7614ed1&_ss=<s1>&_HLS_pathway=cdn-a&_HLS_throughput=6000000

  ... tokens persist for entire session lifetime
```

### CloudFront Signed URL Example

```
SERVER-URI:
  /steer?Policy=eyJ...&Signature=abc...&Key-Pair-Id=APKA...&_ss=<init>

  All three CloudFront signing parameters are preserved automatically.
```

### Which Parameters Are Preserved?

| Preserved (passthrough) | Consumed (not passed through) |
|---|---|
| `token`, `session`, `auth`, etc. | `_HLS_pathway` |
| `start`, `end`, `userId`, `hashParam` | `_HLS_throughput` |
| `Policy`, `Signature`, `Key-Pair-Id` | `_DASH_pathway` |
| Any custom parameter | `_DASH_throughput` |
| | `_ss` (replaced with updated state) |

---

## Player Compatibility

| Player | Protocol | Content Steering Support | Notes |
|--------|----------|--------------------------|-------|
| **AVPlayer** | HLS | Native (iOS 17+, tvOS 17+, macOS 14+) | No code changes needed |
| **HLS.js** | HLS | Native (v1.4+) | Reads `#EXT-X-CONTENT-STEERING` automatically |
| **DASH.js** | DASH | Native (v4.7+) | Reads `<ContentSteering>` from MPD |
| **Shaka Player** | DASH | Native (v4.3+) | Full Content Steering support |
| **Video.js** | Both | Via HLS.js / DASH.js plugins | Depends on underlying player |
| **ExoPlayer** | Both | Partial (v2.19+) | HLS Content Steering support |
