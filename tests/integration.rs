//! Integration tests for apex-edge-steering.
//!
//! These tests exercise the full end-to-end steering flow through the public
//! Rust API (same code paths as the WASM exports), simulating realistic
//! multi-request sessions for both HLS and DASH protocols.

use apex_edge_steering::types::*;
use apex_edge_steering::*;

// ─── Helper: simulate a full steering request cycle ──────────────────────────

/// Simulate what a platform wrapper does: parse query → handle request → return JSON.
fn simulate_request(
    query: &str,
    protocol_hint: &str,
    overrides: &OverrideState,
    config: &PolicyConfig,
    base_path: &str,
) -> (SteeringResponse, String) {
    let parsed = parse_query(query);

    let protocol = parsed.protocol.unwrap_or(match protocol_hint {
        "dash" => Protocol::Dash,
        _ => Protocol::Hls,
    });

    let session_state = match parsed.session_state_raw {
        Some(ref encoded) => decode_state(encoded).ok(),
        None => None,
    };

    // Mirror WASM handler: fall back to stored initial state when _ss is absent.
    let session_state = session_state.unwrap_or_else(|| {
        get_initial_state().unwrap_or_default()
    });

    let passthrough: Vec<(String, String)> = query
        .split('&')
        .filter(|s| !s.is_empty())
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            if key.starts_with("_HLS_")
                || key.starts_with("_DASH_")
                || key == "_ss"
            {
                None
            } else {
                Some((key.to_string(), value.to_string()))
            }
        })
        .collect();

    let resp = build_response(
        protocol,
        &session_state,
        parsed.pathway.as_deref(),
        parsed.throughput,
        overrides,
        config,
        base_path,
        &passthrough,
    )
    .unwrap();

    let json = serde_json::to_string(&resp).unwrap();
    (resp, json)
}

/// Extract the query string from a RELOAD-URI (everything after '?').
fn extract_query(reload_uri: &str) -> &str {
    reload_uri.split_once('?').map(|(_, q)| q).unwrap_or("")
}

// ═════════════════════════════════════════════════════════════════════════════
// Integration Test: HLS multi-request session
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn hls_full_session_lifecycle() {
    let config = PolicyConfig::default();
    let overrides = OverrideState::default();

    // ── Master encodes initial state (sets priorities on edge server) ────
    let initial_state = SessionState {
        priorities: vec!["CDN-A".into(), "CDN-B".into()],
        min_bitrate: 500_000,
        max_bitrate: 4_000_000,
        ..Default::default()
    };
    set_initial_state(&initial_state);

    // ── Request 1: Initial request (no _ss yet, uses stored initial state)
    // Simulates the first call after manifest load with:
    //   SERVER-URI="/steer?session=abc123"
    let (resp1, json1) = simulate_request(
        "session=abc123",
        "hls",
        &overrides,
        &config,
        "/steer",
    );

    assert_eq!(resp1.version, 1);
    assert_eq!(resp1.ttl, 300);
    assert!(resp1.pathway_priority.is_some());
    assert_eq!(resp1.pathway_priority.as_ref().unwrap(), &vec!["CDN-A", "CDN-B"]);
    assert!(resp1.service_location_priority.is_none());
    assert!(resp1.reload_uri.is_some());

    // JSON should have correct HLS format
    let v: serde_json::Value = serde_json::from_str(&json1).unwrap();
    assert_eq!(v["VERSION"], 1);
    assert!(v.get("PATHWAY-PRIORITY").is_some());
    assert!(v.get("SERVICE-LOCATION-PRIORITY").is_none());

    // ── Request 2: Follow-up with pathway and throughput ─────────────────
    // Player is now on a pathway and reporting throughput.
    let reload_query = extract_query(resp1.reload_uri.as_ref().unwrap());
    let q2 = format!("{reload_query}&_HLS_pathway=CDN-A&_HLS_throughput=5000000");

    let (resp2, _) = simulate_request(&q2, "hls", &overrides, &config, "/steer");

    assert_eq!(resp2.version, 1);
    assert!(resp2.reload_uri.is_some());
    // Session token should persist
    assert!(resp2.reload_uri.as_ref().unwrap().contains("session=abc123"));

    // ── Request 3: Another follow-up (state accumulates) ─────────────────
    let reload_query2 = extract_query(resp2.reload_uri.as_ref().unwrap());
    let q3 = format!("{reload_query2}&_HLS_pathway=CDN-A&_HLS_throughput=6000000");

    let (resp3, _) = simulate_request(&q3, "hls", &overrides, &config, "/steer");

    // Decode state from the third response to verify accumulation
    let ss = extract_query(resp3.reload_uri.as_ref().unwrap());
    let ss_encoded = ss.split("_ss=").nth(1).unwrap();
    let state = decode_state(ss_encoded).unwrap();
    // Throughput map should have CDN-A entry
    assert!(state.throughput_map.iter().any(|(p, _)| p == "CDN-A"));
    // Position should have advanced (3 × TTL)
    assert!(state.position > 0);
}

// ═════════════════════════════════════════════════════════════════════════════
// Integration Test: DASH multi-request session
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn dash_full_session_lifecycle() {
    let config = PolicyConfig::default();
    let overrides = OverrideState::default();

    // ── Master encodes initial state ────────────────────────────────────
    let initial_state = SessionState {
        priorities: vec!["alpha".into(), "beta".into()],
        ..Default::default()
    };
    set_initial_state(&initial_state);

    // ── Request 1: queryBeforeStart=true, no pathway yet ─────────────────
    let (resp1, json1) = simulate_request(
        "token=234523452",
        "dash",
        &overrides,
        &config,
        "/steer",
    );

    assert_eq!(resp1.version, 1);
    assert!(resp1.service_location_priority.is_some());
    assert_eq!(resp1.service_location_priority.as_ref().unwrap(), &vec!["alpha", "beta"]);
    assert!(resp1.pathway_priority.is_none());

    let v: serde_json::Value = serde_json::from_str(&json1).unwrap();
    assert!(v.get("SERVICE-LOCATION-PRIORITY").is_some());
    assert!(v.get("PATHWAY-PRIORITY").is_none());

    // ── Request 2: Player reports pathway and throughput ──────────────────
    let reload_query = extract_query(resp1.reload_uri.as_ref().unwrap());
    let q2 = format!("{reload_query}&_DASH_pathway=alpha&_DASH_throughput=5140000");

    let (resp2, _) = simulate_request(&q2, "dash", &overrides, &config, "/steer");

    assert!(resp2.reload_uri.as_ref().unwrap().contains("token=234523452"));
    assert!(resp2.service_location_priority.is_some());
}

// ═════════════════════════════════════════════════════════════════════════════
// Integration Test: QoE-triggered CDN switch
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn qoe_triggered_cdn_switch() {
    let config = PolicyConfig::default();
    let overrides = OverrideState::default();

    // Set up initial state with a known encoding ladder
    let initial_state = SessionState {
        priorities: vec!["cdn-a".into(), "cdn-b".into()],
        min_bitrate: 1_000_000,
        max_bitrate: 8_000_000,
        ..Default::default()
    };
    let encoded = encode_state(&initial_state).unwrap();

    // ── Request 1: Good throughput on cdn-a ──────────────────────────────
    let q1 = format!("_ss={encoded}&_HLS_pathway=cdn-a&_HLS_throughput=5000000");
    let (resp1, _) = simulate_request(&q1, "hls", &overrides, &config, "/steer");

    assert_eq!(resp1.pathway_priority.as_ref().unwrap()[0], "cdn-a");
    assert_eq!(resp1.ttl, 300); // Normal TTL

    // ── Request 2: Degraded throughput on cdn-a ──────────────────────────
    let reload_query = extract_query(resp1.reload_uri.as_ref().unwrap());
    let q2 = format!("{reload_query}&_HLS_pathway=cdn-a&_HLS_throughput=500000");

    let (resp2, _) = simulate_request(&q2, "hls", &overrides, &config, "/steer");

    // cdn-a should be demoted, cdn-b promoted
    assert_eq!(resp2.pathway_priority.as_ref().unwrap()[0], "cdn-b");
    assert_eq!(resp2.ttl, 10); // QoE TTL for fast re-evaluation

    // ── Request 3: Good throughput on cdn-b ──────────────────────────────
    let reload_query2 = extract_query(resp2.reload_uri.as_ref().unwrap());
    let q3 = format!("{reload_query2}&_HLS_pathway=cdn-b&_HLS_throughput=6000000");

    let (resp3, _) = simulate_request(&q3, "hls", &overrides, &config, "/steer");

    // cdn-b stays on top, TTL returns to normal
    assert_eq!(resp3.pathway_priority.as_ref().unwrap()[0], "cdn-b");
    assert_eq!(resp3.ttl, 300);
}

// ═════════════════════════════════════════════════════════════════════════════
// Integration Test: Master override during active session
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn master_override_during_session() {
    let config = PolicyConfig::default();
    let mut overrides = OverrideState::default();

    let initial_state = SessionState {
        priorities: vec!["cdn-a".into(), "cdn-b".into(), "cdn-c".into()],
        min_bitrate: 500_000,
        ..Default::default()
    };
    let encoded = encode_state(&initial_state).unwrap();

    // ── Request 1: Normal operation ──────────────────────────────────────
    let q1 = format!("_ss={encoded}&_HLS_pathway=cdn-a&_HLS_throughput=5000000");
    let (resp1, _) = simulate_request(&q1, "hls", &overrides, &config, "/steer");
    assert_eq!(resp1.pathway_priority.as_ref().unwrap()[0], "cdn-a");

    // ── Master pushes override: force cdn-c as primary ───────────────────
    apply_command(&mut overrides, &ControlCommand::SetPriorities {
        region: None,
        priorities: vec!["cdn-c".into(), "cdn-a".into()],
        generation: 1,
        ttl_override: Some(30),
    });

    // ── Request 2: Next client request picks up the override ─────────────
    let reload_query = extract_query(resp1.reload_uri.as_ref().unwrap());
    let q2 = format!("{reload_query}&_HLS_pathway=cdn-a&_HLS_throughput=5000000");

    let (resp2, _) = simulate_request(&q2, "hls", &overrides, &config, "/steer");

    assert_eq!(resp2.pathway_priority.as_ref().unwrap()[0], "cdn-c");
    assert_eq!(resp2.ttl, 30);
}

// ═════════════════════════════════════════════════════════════════════════════
// Integration Test: Disaster recovery — exclude a CDN
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn disaster_recovery_exclude_cdn() {
    let config = PolicyConfig::default();
    let mut overrides = OverrideState::default();

    let initial_state = SessionState {
        priorities: vec!["cdn-a".into(), "cdn-b".into(), "cdn-c".into()],
        ..Default::default()
    };
    let encoded = encode_state(&initial_state).unwrap();

    // ── Master excludes cdn-a (outage) ───────────────────────────────────
    apply_command(&mut overrides, &ControlCommand::ExcludePathway {
        region: None,
        pathway: "cdn-a".into(),
        generation: 1,
    });

    // ── Client request ───────────────────────────────────────────────────
    let q = format!("_ss={encoded}&_HLS_pathway=cdn-a&_HLS_throughput=0");
    let (resp, _) = simulate_request(&q, "hls", &overrides, &config, "/steer");

    let priorities = resp.pathway_priority.unwrap();
    assert!(!priorities.contains(&"cdn-a".to_string()));
    assert_eq!(priorities[0], "cdn-b");

    // ── Master clears overrides (cdn-a recovered) ────────────────────────
    apply_command(&mut overrides, &ControlCommand::ClearOverrides {
        region: None,
        generation: 2,
    });

    let (resp2, _) = simulate_request(&q, "hls", &overrides, &config, "/steer");
    let priorities2 = resp2.pathway_priority.unwrap();
    assert!(priorities2.contains(&"cdn-a".to_string()));
}

// ═════════════════════════════════════════════════════════════════════════════
// Integration Test: Akamai token passthrough across full session
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn akamai_token_passthrough_full_session() {
    let config = PolicyConfig::default();
    let overrides = OverrideState::default();
    let base = "/steer";

    // ── Master encodes initial state ────────────────────────────────────
    let initial_state = SessionState {
        priorities: vec!["CDN-A".into(), "CDN-B".into()],
        ..Default::default()
    };
    set_initial_state(&initial_state);

    // Initial request with Akamai-style tokens (no _ss, uses stored initial state)
    let q1 = "start=1772770805&end=1772857805&userId=93334984\
              &hashParam=a7614ed13747de0802fdd8ff5cd440b4";

    let (resp1, _) = simulate_request(q1, "hls", &overrides, &config, base);

    let uri1 = resp1.reload_uri.unwrap();
    assert!(uri1.contains("start=1772770805"));
    assert!(uri1.contains("end=1772857805"));
    assert!(uri1.contains("userId=93334984"));
    assert!(uri1.contains("hashParam=a7614ed13747de0802fdd8ff5cd440b4"));

    // Follow-up request using RELOAD-URI
    let q2_base = extract_query(&uri1);
    let q2 = format!("{q2_base}&_HLS_pathway=CDN-A&_HLS_throughput=5000000");

    let (resp2, _) = simulate_request(&q2, "hls", &overrides, &config, base);

    let uri2 = resp2.reload_uri.unwrap();
    // Tokens must persist across all requests
    assert!(uri2.contains("start=1772770805"));
    assert!(uri2.contains("end=1772857805"));
    assert!(uri2.contains("userId=93334984"));
    assert!(uri2.contains("hashParam=a7614ed13747de0802fdd8ff5cd440b4"));

    // Third request — tokens still there
    let q3_base = extract_query(&uri2);
    let q3 = format!("{q3_base}&_HLS_pathway=CDN-A&_HLS_throughput=6000000");

    let (resp3, _) = simulate_request(&q3, "hls", &overrides, &config, base);

    let uri3 = resp3.reload_uri.unwrap();
    assert!(uri3.contains("hashParam=a7614ed13747de0802fdd8ff5cd440b4"));
}

// ═════════════════════════════════════════════════════════════════════════════
// Integration Test: DASH proxy server URL pattern
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn dash_with_steering_token_and_session() {
    let config = PolicyConfig::default();
    let overrides = OverrideState::default();

    // ── Master encodes initial state ────────────────────────────────────
    let initial_state = SessionState {
        priorities: vec!["alpha".into(), "beta".into()],
        ..Default::default()
    };
    set_initial_state(&initial_state);

    // Simulates the DASH-IF Annex A example (no _ss, uses stored initial state)
    let q1 = "token=234523452";
    let (resp1, _) = simulate_request(q1, "dash", &overrides, &config, "/steer");

    assert!(resp1.service_location_priority.is_some());

    let reload = resp1.reload_uri.unwrap();
    assert!(reload.contains("token=234523452"));

    // Second request with pathway
    let q2 = format!(
        "{}&_DASH_pathway=alpha&_DASH_throughput=5140000",
        extract_query(&reload)
    );
    let (resp2, json2) = simulate_request(&q2, "dash", &overrides, &config, "/steer");

    let v: serde_json::Value = serde_json::from_str(&json2).unwrap();
    assert_eq!(v["VERSION"], 1);
    assert!(v["TTL"].as_u64().unwrap() > 0);
    assert!(v["SERVICE-LOCATION-PRIORITY"].is_array());
    assert!(resp2.reload_uri.unwrap().contains("token=234523452"));
}

// ═════════════════════════════════════════════════════════════════════════════
// Integration Test: encode_initial_state for manifest updater
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn initial_state_encoding_for_manifest_updater() {
    // The manifest updater uses encode_state to embed initial state in SERVER-URI
    let state = SessionState {
        priorities: vec!["cdn-a".into(), "cdn-b".into()],
        min_bitrate: 783_322,
        max_bitrate: 4_530_860,
        duration: 596,
        position: 0,
        timestamp: 1700000000,
        override_gen: 0,
        ..Default::default()
    };

    let encoded = encode_state(&state).unwrap();

    // Verify it can be decoded
    let decoded = decode_state(&encoded).unwrap();
    assert_eq!(decoded.priorities, vec!["cdn-a", "cdn-b"]);
    assert_eq!(decoded.min_bitrate, 783_322);
    assert_eq!(decoded.max_bitrate, 4_530_860);
    assert_eq!(decoded.duration, 596);

    // Verify it can be used in a steering request
    let q = format!("_ss={encoded}&_HLS_pathway=cdn-a&_HLS_throughput=3000000");
    let (resp, _) = simulate_request(
        &q,
        "hls",
        &OverrideState::default(),
        &PolicyConfig::default(),
        "/steer",
    );
    assert_eq!(resp.pathway_priority.unwrap(), vec!["cdn-a", "cdn-b"]);
}

// ═════════════════════════════════════════════════════════════════════════════
// Integration Test: Control command JSON round-trip
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn control_command_json_roundtrip() {
    let mut overrides = OverrideState::default();

    // Apply via JSON (simulating what the WASM apply_control_command does)
    let cmd_json = r#"{
        "type": "set_priorities",
        "region": null,
        "priorities": ["cdn-b", "cdn-a"],
        "generation": 1,
        "ttl_override": 15
    }"#;
    let cmd: ControlCommand = serde_json::from_str(cmd_json).unwrap();
    apply_command(&mut overrides, &cmd);

    // Serialize overrides and deserialize (simulating WASM boundary)
    let overrides_json = serde_json::to_string(&overrides).unwrap();
    let overrides_back: OverrideState = serde_json::from_str(&overrides_json).unwrap();

    assert_eq!(overrides_back.generation, 1);
    assert!(overrides_back.priority_override.is_some());
    let ov = overrides_back.priority_override.unwrap();
    assert_eq!(ov.priorities, vec!["cdn-b", "cdn-a"]);
    assert_eq!(ov.ttl_override, Some(15));
}

// ═════════════════════════════════════════════════════════════════════════════
// Integration Test: Master override persists across multi-hop session
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn master_override_persists_across_multi_hop() {
    let config = PolicyConfig::default();
    let mut overrides = OverrideState::default();

    // Initial session state: cdn-a is primary
    let initial_state = SessionState {
        priorities: vec!["cdn-a".into(), "cdn-b".into(), "cdn-c".into()],
        min_bitrate: 1_000_000,
        max_bitrate: 8_000_000,
        ..Default::default()
    };
    let encoded = encode_state(&initial_state).unwrap();

    // ── Request 1: Before any override — client state priorities used ────
    let q1 = format!("_ss={encoded}&_HLS_pathway=cdn-a&_HLS_throughput=5000000");
    let (resp1, _) = simulate_request(&q1, "hls", &overrides, &config, "/steer");
    assert_eq!(resp1.pathway_priority.as_ref().unwrap()[0], "cdn-a");
    assert_eq!(resp1.ttl, 300);

    // ── Master pushes override: force cdn-c as primary ───────────────────
    apply_command(&mut overrides, &ControlCommand::SetPriorities {
        region: None,
        priorities: vec!["cdn-c".into(), "cdn-b".into(), "cdn-a".into()],
        generation: 1,
        ttl_override: Some(30),
    });

    // ── Request 2: Client uses RELOAD-URI from request 1 — override applied
    let q2 = format!(
        "{}&_HLS_pathway=cdn-a&_HLS_throughput=5000000",
        extract_query(resp1.reload_uri.as_ref().unwrap())
    );
    let (resp2, _) = simulate_request(&q2, "hls", &overrides, &config, "/steer");
    assert_eq!(resp2.pathway_priority.as_ref().unwrap()[0], "cdn-c");
    assert_eq!(resp2.ttl, 30);

    // ── Request 3: Client uses RELOAD-URI from request 2 — override STILL applied
    let q3 = format!(
        "{}&_HLS_pathway=cdn-c&_HLS_throughput=6000000",
        extract_query(resp2.reload_uri.as_ref().unwrap())
    );
    let (resp3, _) = simulate_request(&q3, "hls", &overrides, &config, "/steer");
    assert_eq!(resp3.pathway_priority.as_ref().unwrap()[0], "cdn-c");
    assert_eq!(resp3.ttl, 30);

    // Verify state in RELOAD-URI carries override priorities
    let ss_encoded = extract_query(resp3.reload_uri.as_ref().unwrap())
        .split("_ss=").nth(1).unwrap().split('&').next().unwrap();
    let state = decode_state(ss_encoded).unwrap();
    assert_eq!(state.priorities[0], "cdn-c");
    assert_eq!(state.override_gen, 1);

    // ── Master pushes NEW override gen=2 ─────────────────────────────────
    apply_command(&mut overrides, &ControlCommand::SetPriorities {
        region: None,
        priorities: vec!["cdn-b".into(), "cdn-a".into()],
        generation: 2,
        ttl_override: Some(15),
    });

    // ── Request 4: Client uses RELOAD-URI from request 3 — NEW override applied
    let q4 = format!(
        "{}&_HLS_pathway=cdn-c&_HLS_throughput=6000000",
        extract_query(resp3.reload_uri.as_ref().unwrap())
    );
    let (resp4, _) = simulate_request(&q4, "hls", &overrides, &config, "/steer");
    assert_eq!(
        resp4.pathway_priority.unwrap(),
        vec!["cdn-b", "cdn-a"]
    );
    assert_eq!(resp4.ttl, 15);
}

// ═════════════════════════════════════════════════════════════════════════════
// Integration Test: Master override wins even when client state has same gen
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn master_override_applied_when_client_state_has_equal_override_gen() {
    let config = PolicyConfig::default();

    // Simulate: client processed override gen=1 on a previous request,
    // state now has override_gen=1. The SAME override (gen=1) is still
    // active on the edge. It must still be applied.
    let state = SessionState {
        priorities: vec!["cdn-b".into(), "cdn-a".into()], // from the override
        override_gen: 1,
        min_bitrate: 1_000_000,
        max_bitrate: 8_000_000,
        ..Default::default()
    };
    let encoded = encode_state(&state).unwrap();

    let overrides = OverrideState {
        priority_override: Some(PriorityOverride {
            priorities: vec!["cdn-b".into(), "cdn-a".into()],
            generation: 1,
            ttl_override: Some(30),
        }),
        generation: 1,
        ..Default::default()
    };

    let q = format!("_ss={encoded}&_HLS_pathway=cdn-b&_HLS_throughput=5000000");
    let (resp, _) = simulate_request(&q, "hls", &overrides, &config, "/steer");

    // Override TTL should still be applied (proves override is active)
    assert_eq!(resp.pathway_priority.unwrap(), vec!["cdn-b", "cdn-a"]);
    assert_eq!(resp.ttl, 30);
}

// ═════════════════════════════════════════════════════════════════════════════
// Integration Test: Concurrent viewers with different CDN assignments
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn concurrent_viewers_independent_state() {
    let config = PolicyConfig::default();
    let overrides = OverrideState::default();

    // Viewer 1: assigned to cdn-a
    let state1 = SessionState {
        priorities: vec!["cdn-a".into(), "cdn-b".into()],
        min_bitrate: 1_000_000,
        ..Default::default()
    };
    let enc1 = encode_state(&state1).unwrap();

    // Viewer 2: assigned to cdn-b (load balanced by master)
    let state2 = SessionState {
        priorities: vec!["cdn-b".into(), "cdn-a".into()],
        min_bitrate: 1_000_000,
        ..Default::default()
    };
    let enc2 = encode_state(&state2).unwrap();

    // Both viewers make requests
    let q1 = format!("_ss={enc1}&_HLS_pathway=cdn-a&_HLS_throughput=5000000");
    let q2 = format!("_ss={enc2}&_HLS_pathway=cdn-b&_HLS_throughput=4000000");

    let (resp1, _) = simulate_request(&q1, "hls", &overrides, &config, "/steer");
    let (resp2, _) = simulate_request(&q2, "hls", &overrides, &config, "/steer");

    // Each viewer gets their own priority order
    assert_eq!(resp1.pathway_priority.as_ref().unwrap()[0], "cdn-a");
    assert_eq!(resp2.pathway_priority.as_ref().unwrap()[0], "cdn-b");

    // States are independent
    let ss1 = extract_query(resp1.reload_uri.as_ref().unwrap());
    let ss2 = extract_query(resp2.reload_uri.as_ref().unwrap());
    assert_ne!(ss1, ss2);
}
