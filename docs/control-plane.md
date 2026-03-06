# Control Plane Guide

This guide covers how a **master steering server** (or operations team) controls the edge steering servers via the control plane API. Includes usage examples for all command types, disaster recovery runbooks, and operational patterns.

---

## Table of Contents

1. [Overview](#overview)
2. [Command Types](#command-types)
3. [Usage Examples](#usage-examples)
4. [Disaster Recovery Runbook](#disaster-recovery-runbook)
5. [Operational Patterns](#operational-patterns)
6. [Generation Number Strategy](#generation-number-strategy)
7. [Monitoring Override State](#monitoring-override-state)

---

## Overview

The control plane allows a master steering server to push runtime overrides to edge servers. This is how you:

- **Rebalance traffic** across CDNs (cost optimization, contract commits)
- **Exclude a CDN** during outages (disaster recovery)
- **Override priorities** for maintenance windows
- **Clear overrides** when conditions return to normal

```
┌──────────────────┐     POST /control      ┌──────────────────┐
│                  │ ────────────────────>   │                  │
│  Master Server   │     command JSON        │  Edge Server     │
│  (your backend)  │                         │  (apex-steering) │
│                  │ <────────────────────   │                  │
│                  │     updated overrides   │                  │
└──────────────────┘                         └──────────────────┘

Commands:
  set_priorities   -- Force a specific CDN order
  exclude_pathway  -- Remove a CDN from all responses
  clear_overrides  -- Revert to session-default priorities
```

All commands use **generation numbers** for idempotent, replay-safe processing. A command is rejected if its generation is not strictly greater than the current state.

---

## Command Types

### set_priorities

Force a specific CDN priority order for all sessions.

```json
{
  "type": "set_priorities",
  "region": "us-east",
  "priorities": ["cdn-b", "cdn-a", "cdn-c"],
  "generation": 1,
  "ttl_override": 30
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | `string` | Yes | Must be `"set_priorities"` |
| `region` | `string?` | No | Region filter. `null` = apply globally. |
| `priorities` | `string[]` | Yes | New CDN priority order. First element = preferred CDN. |
| `generation` | `u64` | Yes | Must be > current generation. |
| `ttl_override` | `u32?` | No | Override TTL in seconds. `null` = use config default (300s). |

### exclude_pathway

Remove a CDN from all steering responses. Used for disaster recovery.

```json
{
  "type": "exclude_pathway",
  "region": null,
  "pathway": "cdn-a",
  "generation": 2
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | `string` | Yes | Must be `"exclude_pathway"` |
| `region` | `string?` | No | Region filter. `null` = apply globally. |
| `pathway` | `string` | Yes | CDN pathway to exclude. |
| `generation` | `u64` | Yes | Must be > current generation. |

### clear_overrides

Remove all overrides and revert to session-default priorities.

```json
{
  "type": "clear_overrides",
  "region": null,
  "generation": 3
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | `string` | Yes | Must be `"clear_overrides"` |
| `region` | `string?` | No | Region filter. `null` = apply globally. |
| `generation` | `u64` | Yes | Must be > current generation. |

---

## Usage Examples

### cURL: Set Priorities

```bash
# Force cdn-b as primary, cdn-a as fallback
curl -X POST https://steer.example.com/control \
  -H "Content-Type: application/json" \
  -d '{
    "type": "set_priorities",
    "region": null,
    "priorities": ["cdn-b", "cdn-a"],
    "generation": 1,
    "ttl_override": 30
  }'

# Response: updated override state
# {
#   "priority_override": {
#     "priorities": ["cdn-b", "cdn-a"],
#     "generation": 1,
#     "ttl_override": 30
#   },
#   "excluded_pathways": [],
#   "generation": 1
# }
```

### cURL: Exclude a CDN

```bash
# Remove cdn-a from all responses (CDN outage)
curl -X POST https://steer.example.com/control \
  -H "Content-Type: application/json" \
  -d '{
    "type": "exclude_pathway",
    "region": null,
    "pathway": "cdn-a",
    "generation": 2
  }'
```

### cURL: Clear All Overrides

```bash
# Return to normal operation
curl -X POST https://steer.example.com/control \
  -H "Content-Type: application/json" \
  -d '{
    "type": "clear_overrides",
    "region": null,
    "generation": 3
  }'
```

### Python: Master Server Integration

```python
import requests
import time

EDGE_SERVERS = [
    "https://steer-us-east.example.com",
    "https://steer-us-west.example.com",
    "https://steer-eu-west.example.com",
]

class SteeringMaster:
    def __init__(self):
        self.generation = 0

    def _next_gen(self):
        self.generation += 1
        return self.generation

    def set_priorities(self, priorities, region=None, ttl_override=None):
        """Push a priority override to all edge servers."""
        command = {
            "type": "set_priorities",
            "region": region,
            "priorities": priorities,
            "generation": self._next_gen(),
            "ttl_override": ttl_override,
        }
        return self._broadcast(command)

    def exclude_pathway(self, pathway, region=None):
        """Exclude a CDN pathway from all steering responses."""
        command = {
            "type": "exclude_pathway",
            "region": region,
            "pathway": pathway,
            "generation": self._next_gen(),
        }
        return self._broadcast(command)

    def clear_overrides(self, region=None):
        """Clear all overrides, revert to session defaults."""
        command = {
            "type": "clear_overrides",
            "region": region,
            "generation": self._next_gen(),
        }
        return self._broadcast(command)

    def _broadcast(self, command):
        """Send command to all edge servers."""
        results = {}
        for server in EDGE_SERVERS:
            try:
                resp = requests.post(
                    f"{server}/control",
                    json=command,
                    timeout=5,
                )
                results[server] = {
                    "status": resp.status_code,
                    "body": resp.json(),
                }
            except Exception as e:
                results[server] = {"error": str(e)}
        return results


# Usage:
master = SteeringMaster()

# Rebalance: move traffic to cdn-b
master.set_priorities(["cdn-b", "cdn-a", "cdn-c"], ttl_override=30)

# CDN-A outage detected
master.exclude_pathway("cdn-a")

# CDN-A recovered
master.clear_overrides()
```

### Node.js: Master Server Integration

```javascript
const EDGE_SERVERS = [
  'https://steer-us-east.example.com',
  'https://steer-us-west.example.com',
  'https://steer-eu-west.example.com',
];

let generation = 0;

async function sendCommand(command) {
  const results = await Promise.allSettled(
    EDGE_SERVERS.map(server =>
      fetch(`${server}/control`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(command),
      }).then(r => r.json())
    )
  );
  return results;
}

// Rebalance traffic to cdn-b
await sendCommand({
  type: 'set_priorities',
  region: null,
  priorities: ['cdn-b', 'cdn-a'],
  generation: ++generation,
  ttl_override: 30,
});

// Exclude cdn-a during outage
await sendCommand({
  type: 'exclude_pathway',
  region: null,
  pathway: 'cdn-a',
  generation: ++generation,
});

// Clear all overrides
await sendCommand({
  type: 'clear_overrides',
  region: null,
  generation: ++generation,
});
```

### Rust: Direct API Usage (non-WASM)

```rust
use apex_steering::{apply_command, types::*};

let mut overrides = OverrideState::default();

// Force cdn-b as primary
apply_command(&mut overrides, &ControlCommand::SetPriorities {
    region: None,
    priorities: vec!["cdn-b".into(), "cdn-a".into()],
    generation: 1,
    ttl_override: Some(30),
});

// Exclude cdn-a
apply_command(&mut overrides, &ControlCommand::ExcludePathway {
    region: None,
    pathway: "cdn-a".into(),
    generation: 2,
});

// Clear everything
apply_command(&mut overrides, &ControlCommand::ClearOverrides {
    region: None,
    generation: 3,
});
```

---

## Disaster Recovery Runbook

### Scenario: CDN Outage

```
Timeline:
  t=0     CDN-A starts returning 5xx errors
  t=30s   Monitoring detects elevated error rate
  t=45s   Master server sends exclude_pathway command
  t=46s   Edge servers stop including cdn-a in responses
  t=46s+  Players on cdn-a get TTL=10 or TTL=300 response on next poll
          - With TTL=300: worst case 5 min until switch
          - With ttl_override=10: 10 seconds to switch
  t=???   CDN-A recovers
  t=???   Master sends clear_overrides
  t=???   Normal traffic distribution resumes
```

**Step-by-step:**

```bash
# 1. EXCLUDE the failing CDN immediately
curl -X POST https://steer.example.com/control \
  -d '{"type":"exclude_pathway","region":null,"pathway":"cdn-a","generation":1}'

# 2. Optionally SET PRIORITIES to force a specific failover order
#    with a short TTL for faster convergence
curl -X POST https://steer.example.com/control \
  -d '{"type":"set_priorities","region":null,"priorities":["cdn-b","cdn-c"],"generation":2,"ttl_override":10}'

# 3. Monitor: check health endpoint to verify edge servers are responsive
curl https://steer.example.com/health
# {"status":"ok","engine":"apex-steering"}

# 4. CLEAR when CDN recovers
curl -X POST https://steer.example.com/control \
  -d '{"type":"clear_overrides","region":null,"generation":3}'
```

### Scenario: Planned CDN Maintenance

```bash
# 1. Before maintenance window: gradually move traffic away
curl -X POST https://steer.example.com/control \
  -d '{"type":"set_priorities","region":null,"priorities":["cdn-b","cdn-c","cdn-a"],"generation":1,"ttl_override":null}'

# 2. Wait for TTL to expire (up to 5 minutes for all sessions to pick up new priorities)

# 3. Exclude the CDN under maintenance
curl -X POST https://steer.example.com/control \
  -d '{"type":"exclude_pathway","region":null,"pathway":"cdn-a","generation":2}'

# 4. Perform maintenance...

# 5. After maintenance: clear and return to normal
curl -X POST https://steer.example.com/control \
  -d '{"type":"clear_overrides","region":null,"generation":3}'
```

### Scenario: Cost-Based Rebalancing

```bash
# Approaching CDN-A contract commit -- shift more traffic there
curl -X POST https://steer.example.com/control \
  -d '{"type":"set_priorities","region":null,"priorities":["cdn-a","cdn-b","cdn-c"],"generation":1,"ttl_override":null}'

# Contract commit met -- return to balanced routing
curl -X POST https://steer.example.com/control \
  -d '{"type":"clear_overrides","region":null,"generation":2}'
```

---

## Operational Patterns

### Heartbeat Push

Override state lives in-memory in each edge worker instance. New instances start with empty overrides. To ensure active overrides persist across worker restarts:

```
Master Server Heartbeat Loop:

  every 60 seconds:
    for each edge server:
      POST /control with current active command
      (same generation number -- will be idempotent)
```

Because commands with `generation <= current` are rejected, re-sending the same command is safe and has no effect. Only new commands (with higher generation) will be applied.

### Regional Targeting

Commands support an optional `region` field. While apex-steering doesn't enforce region filtering at the edge level (the edge server applies all commands), this field enables:

1. **Routing-based isolation** -- different edge server deployments per region
2. **Master-side filtering** -- master only pushes relevant commands to each region's edge servers
3. **Audit trail** -- track which regions were affected by each command

```bash
# Only affect US-East edge servers
curl -X POST https://steer-us-east.example.com/control \
  -d '{"type":"set_priorities","region":"us-east","priorities":["cdn-b","cdn-a"],"generation":1,"ttl_override":null}'
```

### Gradual Rollout

For large-scale CDN changes, use a staged approach:

```
Phase 1: Override priorities with long TTL (no urgency)
  {"type":"set_priorities","priorities":["cdn-b","cdn-a"],"generation":1,"ttl_override":null}

Phase 2: Verify metrics (error rates, throughput, QoE scores)

Phase 3: If issues detected, clear overrides immediately
  {"type":"clear_overrides","generation":2}

Phase 3 (alt): If successful, this is the new default.
  Update manifest updater to embed new default priorities.
  Then clear overrides.
  {"type":"clear_overrides","generation":2}
```

---

## Generation Number Strategy

### Requirements

- Must be **monotonically increasing** across all commands
- Must be **globally unique** (if multiple masters exist, coordinate generation space)

### Recommended Approaches

| Strategy | Example | Pros | Cons |
|----------|---------|------|------|
| Unix timestamp (seconds) | `1700000001` | Simple, globally ordered | 1 command/second max |
| Unix timestamp (millis) | `1700000001234` | High resolution | Large numbers |
| Sequential counter | `1, 2, 3, ...` | Simple, compact | Requires persistent storage |
| Hybrid (timestamp + seq) | `170000000100` | Best of both | Slightly complex |

### What Happens with Stale Commands

```
Current state: generation = 5

Command: {"generation": 3}  --> REJECTED (3 <= 5, stale)
Command: {"generation": 5}  --> REJECTED (5 <= 5, equal)
Command: {"generation": 6}  --> APPLIED (6 > 5)
Command: {"generation": 6}  --> REJECTED (6 <= 6, replay)
```

This makes the system **idempotent** -- you can safely retry commands without side effects.

---

## Monitoring Override State

### Check Current State

The `POST /control` response returns the full current override state:

```json
{
  "priority_override": {
    "priorities": ["cdn-b", "cdn-a"],
    "generation": 5,
    "ttl_override": 30
  },
  "excluded_pathways": ["cdn-c"],
  "generation": 5
}
```

### Health Check

```bash
curl https://steer.example.com/health
# {"status":"ok","engine":"apex-steering"}
```

### Override State Lifecycle

```
Worker Start     Command Applied     Worker Restart    Command Re-pushed
     │                │                    │                  │
     v                v                    v                  v
  ┌────────┐    ┌──────────┐         ┌────────┐       ┌──────────┐
  │ Empty  │───>│ Override │ ─ ─ ─ > │ Empty  │ ────> │ Override │
  │ State  │    │ Active   │  lost   │ State  │       │ Active   │
  └────────┘    └──────────┘         └────────┘       └──────────┘

  Override state is in-memory only.
  Master must re-push active overrides to new worker instances.
  Heartbeat pattern (see above) handles this automatically.
```
