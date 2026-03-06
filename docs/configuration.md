# Configuration & Tuning

How to configure the policy engine, tune QoE parameters, and customize behavior.

---

## Table of Contents

1. [Policy Configuration](#policy-configuration)
2. [QoE Optimization](#qoe-optimization)
3. [TTL Tuning](#ttl-tuning)
4. [Configuration Delivery](#configuration-delivery)
5. [Initial Session State](#initial-session-state)

---

## Policy Configuration

The policy engine is configured via `PolicyConfig`, passed as JSON to `handle_steering_request`.

### Default Configuration

```json
{
  "default_ttl": 300,
  "qoe_ttl": 10,
  "degradation_factor": 1.2,
  "qoe_enabled": true
}
```

### Fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `default_ttl` | `u32` | `300` | Normal polling interval in seconds. Per both HLS and DASH spec recommendations. |
| `qoe_ttl` | `u32` | `10` | Fast polling interval during active QoE degradation. Shorter = faster recovery. |
| `degradation_factor` | `f64` | `1.2` | Throughput threshold multiplier. Degraded if `throughput < factor * min_bitrate`. |
| `qoe_enabled` | `bool` | `true` | Master switch for QoE-based CDN switching. |

### Passing Configuration

**Option 1: Empty string (use defaults)**

```javascript
const response = handle_steering_request(requestJson, overridesJson, '', '/steer');
```

**Option 2: Custom JSON**

```javascript
const config = JSON.stringify({
  default_ttl: 120,        // More frequent polling
  qoe_ttl: 5,             // Very fast degradation recovery
  degradation_factor: 1.5, // More aggressive degradation detection
  qoe_enabled: true,
});

const response = handle_steering_request(requestJson, overridesJson, config, '/steer');
```

---

## QoE Optimization

### How It Works

The QoE engine monitors client-reported throughput and demotes CDNs that can't sustain the minimum rendition bitrate.

```
┌───────────────────────────────────────────────────────────────┐
│                     QoE Decision Logic                         │
│                                                                │
│  Inputs:                                                       │
│    - client_throughput: reported by player (_HLS_throughput)   │
│    - min_bitrate: from session state (encoding ladder min)     │
│    - degradation_factor: from config (default 1.2)             │
│                                                                │
│  Threshold = degradation_factor * min_bitrate                  │
│            = 1.2 * 783,322                                     │
│            = 939,986 bps                                       │
│                                                                │
│  If throughput < threshold:                                    │
│    -> CDN is DEGRADED                                          │
│    -> Swap top two priorities (demote current, promote next)   │
│    -> Set TTL = qoe_ttl (10s) for fast re-evaluation           │
│                                                                │
│  If throughput >= threshold:                                   │
│    -> CDN is HEALTHY                                           │
│    -> Keep current priorities                                  │
│    -> Set TTL = default_ttl (300s)                             │
└───────────────────────────────────────────────────────────────┘
```

### QoE Constraints

QoE optimization is **skipped** when any of these conditions apply:

| Condition | Reason |
|-----------|--------|
| `qoe_enabled = false` | Explicitly disabled in config |
| `min_bitrate = 0` | Encoding ladder unknown; can't compute threshold |
| Client reports on non-top pathway | Only demote the top-priority CDN |
| Only 1 pathway available | Nowhere to failover |
| Throughput >= threshold | CDN is performing adequately |

### Tuning the Degradation Factor

The `degradation_factor` controls how sensitive the QoE engine is:

| Factor | Behavior | Use Case |
|--------|----------|----------|
| `1.0` | Degrade if throughput < min_bitrate | Very conservative -- only trigger on hard failures |
| `1.2` (default) | Degrade if throughput < 1.2x min_bitrate | Balanced -- 20% headroom |
| `1.5` | Degrade if throughput < 1.5x min_bitrate | Aggressive -- switch early |
| `2.0` | Degrade if throughput < 2x min_bitrate | Very aggressive -- switch at any sign of congestion |

**Example with a typical encoding ladder:**

```
min_bitrate = 783,322 bps (480p)

Factor 1.0:  threshold =   783,322 bps (switch only if can't sustain 480p)
Factor 1.2:  threshold =   939,986 bps (switch if barely above 480p)
Factor 1.5:  threshold = 1,174,983 bps (switch if below ~720p)
Factor 2.0:  threshold = 1,566,644 bps (switch if not comfortably above 480p)
```

### QoE Recovery

After a CDN switch, the new CDN gets a chance to prove itself:

```
Request N:   cdn-a throughput = 500K  -> DEGRADED -> promote cdn-b, TTL=10s
Request N+1: cdn-b throughput = 6M   -> HEALTHY  -> keep cdn-b, TTL=300s
                                                      ^^ normal TTL restored
```

If the new CDN is also degraded, it will be demoted too on the next cycle (with TTL=10s for fast iteration).

---

## TTL Tuning

### Default TTL (`default_ttl`)

**Default: 300 seconds (5 minutes)**

This is the normal polling interval per both HLS and DASH spec recommendations. Affects:
- How quickly sessions pick up new master overrides
- How quickly QoE recovers after degradation ends
- Network overhead (one small JSON request per session per TTL interval)

| Value | Trade-off |
|-------|-----------|
| `60` | Fast response to changes; higher request volume |
| `120` | Good balance for active content steering |
| `300` (default) | Spec-recommended; minimal overhead |
| `600` | Very low overhead; slow to react to changes |

### QoE TTL (`qoe_ttl`)

**Default: 10 seconds**

Used during active QoE degradation for fast re-evaluation.

| Value | Trade-off |
|-------|-----------|
| `5` | Very fast recovery; highest request volume during incidents |
| `10` (default) | Good balance between speed and load |
| `30` | Slower recovery; lower load during incidents |

### TTL Override (via control plane)

Master server can override TTL for all sessions via `set_priorities`:

```json
{
  "type": "set_priorities",
  "priorities": ["cdn-b", "cdn-a"],
  "generation": 1,
  "ttl_override": 15
}
```

This forces all sessions to poll every 15 seconds until the override is cleared. Useful for:
- Ensuring fast convergence after a CDN switch
- Monitoring during a maintenance window
- Rapid rollback capability during changes

---

## Configuration Delivery

### Static (Inline)

Hardcode config in the wrapper:

```javascript
const configJson = JSON.stringify({
  default_ttl: 300,
  qoe_ttl: 10,
  degradation_factor: 1.2,
  qoe_enabled: true,
});
```

### Akamai EdgeKV

```javascript
import { EdgeKV } from './edgekv.js';
const edgeKv = new EdgeKV({ namespace: 'steering', group: 'config' });

async function loadConfig() {
  try {
    return await edgeKv.getText({ item: 'policy' });
  } catch {
    return ''; // fall back to defaults
  }
}
```

### CloudFront (DynamoDB via Lambda)

```javascript
const { DynamoDBClient, GetItemCommand } = require('@aws-sdk/client-dynamodb');
const client = new DynamoDBClient({});

async function loadConfig() {
  const result = await client.send(new GetItemCommand({
    TableName: 'steering-config',
    Key: { id: { S: 'policy' } },
  }));
  return result.Item?.config?.S || '';
}
```

### Cloudflare KV

```javascript
export default {
  async fetch(request, env) {
    const configJson = await env.STEERING_CONFIG.get('policy') || '';
    // ... use configJson in handle_steering_request
  }
};
```

---

## Initial Session State

The manifest updater sets up the initial `SessionState` embedded in `SERVER-URI`. Proper initialization affects the entire session.

### Required Fields

| Field | How to Determine | Impact |
|-------|------------------|--------|
| `priorities` | Master server decides based on load balancing, region, cost | Determines initial CDN routing |
| `min_bitrate` | Lowest bitrate variant in the encoding ladder | Required for QoE threshold calculation |
| `max_bitrate` | Highest bitrate variant | Informational |

### Optional Fields

| Field | Default | When to Set |
|-------|---------|-------------|
| `duration` | `0` | Set for VOD content (enables position tracking) |
| `position` | `0` | Always start at 0 |
| `timestamp` | `0` | Set to current epoch for session age tracking |
| `override_gen` | `0` | Always start at 0 |
| `throughput_map` | `[]` | Always start empty (populated from client reports) |

### Example: Proper Initialization

```javascript
const state = {
  // Master decides CDN order based on region, cost, load
  priorities: masterServer.getPrioritiesForRegion(userRegion),

  // From encoding ladder metadata
  min_bitrate: encodingLadder.renditions[encodingLadder.renditions.length - 1].bandwidth,
  max_bitrate: encodingLadder.renditions[0].bandwidth,

  // Content metadata
  duration: contentDuration, // 0 for live
  position: 0,
  timestamp: Math.floor(Date.now() / 1000),

  // Always start clean
  throughput_map: [],
  override_gen: 0,
};

const encoded = encode_initial_state(JSON.stringify(state));
const serverUri = `/steer?session=${sessionId}&_ss=${encoded}`;
```

### Impact of Missing Fields

| Missing Field | Consequence |
|--------------|-------------|
| `min_bitrate = 0` | QoE optimization disabled (can't compute threshold) |
| `priorities = []` | Empty response priorities (player may error) |
| `timestamp = 0` | Session age tracking unavailable |
| `duration = 0` | Treated as live content (position tracking less meaningful) |
