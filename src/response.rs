use crate::types::{Protocol, SessionState, SteeringResponse};
use crate::state;

/// Build a complete steering response with an encoded RELOAD-URI.
///
/// This is the main entry point called by the request handler. It:
/// 1. Runs the policy engine to determine priorities and TTL
/// 2. Updates the session state with new throughput data
/// 3. Encodes the updated state into the RELOAD-URI
/// 4. Returns the final JSON-serializable response
pub fn build_response(
    protocol: Protocol,
    session_state: &SessionState,
    client_pathway: Option<&str>,
    client_throughput: Option<u64>,
    overrides: &crate::types::OverrideState,
    config: &crate::policy::PolicyConfig,
    base_path: &str,
    passthrough_params: &[(String, String)],
) -> Result<SteeringResponse, String> {
    // Run policy evaluation.
    let mut resp = crate::policy::evaluate(
        protocol,
        session_state,
        client_pathway,
        client_throughput,
        overrides,
        config,
    );

    // Build updated session state to carry forward.
    let mut next_state = session_state.clone();

    // Update priorities to match what we told the client.
    let priorities = match protocol {
        Protocol::Hls => resp.pathway_priority.clone().unwrap_or_default(),
        Protocol::Dash => resp.service_location_priority.clone().unwrap_or_default(),
    };
    next_state.priorities = priorities;

    // Update throughput map with latest client report.
    if let (Some(pathway), Some(throughput)) = (client_pathway, client_throughput) {
        update_throughput_map(&mut next_state.throughput_map, pathway, throughput);
    }

    // Advance position estimate based on elapsed time and TTL.
    next_state.position = next_state.position.saturating_add(resp.ttl as u64);

    // Track override generation.
    if let Some(ref ov) = overrides.priority_override {
        next_state.override_gen = next_state.override_gen.max(ov.generation);
    }

    // Encode state into RELOAD-URI.
    let reload_uri = state::build_reload_uri(base_path, &next_state, passthrough_params)?;
    resp.reload_uri = Some(reload_uri);

    Ok(resp)
}

/// Update the throughput map with the latest observation.
/// Keeps at most one entry per pathway (most recent wins).
fn update_throughput_map(map: &mut Vec<(String, u64)>, pathway: &str, throughput: u64) {
    if let Some(entry) = map.iter_mut().find(|(p, _)| p == pathway) {
        entry.1 = throughput;
    } else {
        map.push((pathway.to_string(), throughput));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::PolicyConfig;
    use crate::types::{OverrideState, PriorityOverride};

    // ─── HLS response building ──────────────────────────────────────────

    #[test]
    fn build_response_hls_basic() {
        let state = SessionState {
            priorities: vec!["cdn-a".into(), "cdn-b".into()],
            min_bitrate: 500_000,
            max_bitrate: 4_000_000,
            ..Default::default()
        };
        let resp = build_response(
            Protocol::Hls,
            &state,
            Some("cdn-a"),
            Some(3_000_000),
            &OverrideState::default(),
            &PolicyConfig::default(),
            "/steer",
            &[("session".into(), "abc".into())],
        )
        .unwrap();

        assert_eq!(resp.version, 1);
        assert!(resp.reload_uri.is_some());
        assert!(resp.reload_uri.as_ref().unwrap().contains("_ss="));
        assert!(resp.reload_uri.as_ref().unwrap().contains("session=abc"));
        assert!(resp.pathway_priority.is_some());
        assert!(resp.service_location_priority.is_none());
    }

    // ─── DASH response building ─────────────────────────────────────────

    #[test]
    fn build_response_dash_basic() {
        let state = SessionState {
            priorities: vec!["alpha".into(), "beta".into()],
            ..Default::default()
        };
        let resp = build_response(
            Protocol::Dash,
            &state,
            None,
            None,
            &OverrideState::default(),
            &PolicyConfig::default(),
            "/steer",
            &[],
        )
        .unwrap();

        assert!(resp.pathway_priority.is_none());
        assert!(resp.service_location_priority.is_some());
        assert_eq!(resp.service_location_priority.unwrap(), vec!["alpha", "beta"]);
    }

    // ─── State carried through RELOAD-URI ───────────────────────────────

    #[test]
    fn throughput_carried_in_state() {
        let state = SessionState {
            priorities: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        let resp = build_response(
            Protocol::Hls,
            &state,
            Some("a"),
            Some(5_000_000),
            &OverrideState::default(),
            &PolicyConfig::default(),
            "/steer",
            &[],
        )
        .unwrap();

        let uri = resp.reload_uri.unwrap();
        let ss_param = uri.split("_ss=").nth(1).unwrap();
        let decoded = crate::state::decode_state(ss_param).unwrap();
        assert!(decoded.throughput_map.iter().any(|(p, t)| p == "a" && *t == 5_000_000));
    }

    #[test]
    fn throughput_map_updates_existing_entry() {
        let state = SessionState {
            priorities: vec!["a".into(), "b".into()],
            throughput_map: vec![("a".into(), 1_000_000)],
            ..Default::default()
        };
        let resp = build_response(
            Protocol::Hls,
            &state,
            Some("a"),
            Some(8_000_000),
            &OverrideState::default(),
            &PolicyConfig::default(),
            "/steer",
            &[],
        )
        .unwrap();

        let ss_param = resp.reload_uri.unwrap().split("_ss=").nth(1).unwrap().to_string();
        let decoded = crate::state::decode_state(&ss_param).unwrap();
        // Should have updated the existing entry, not added a new one
        let a_entries: Vec<_> = decoded.throughput_map.iter().filter(|(p, _)| p == "a").collect();
        assert_eq!(a_entries.len(), 1);
        assert_eq!(a_entries[0].1, 8_000_000);
    }

    #[test]
    fn throughput_map_adds_new_pathway() {
        let state = SessionState {
            priorities: vec!["a".into(), "b".into()],
            throughput_map: vec![("a".into(), 1_000_000)],
            ..Default::default()
        };
        let resp = build_response(
            Protocol::Hls,
            &state,
            Some("b"),
            Some(3_000_000),
            &OverrideState::default(),
            &PolicyConfig::default(),
            "/steer",
            &[],
        )
        .unwrap();

        let ss_param = resp.reload_uri.unwrap().split("_ss=").nth(1).unwrap().to_string();
        let decoded = crate::state::decode_state(&ss_param).unwrap();
        assert_eq!(decoded.throughput_map.len(), 2);
    }

    #[test]
    fn position_advances_by_ttl() {
        let state = SessionState {
            priorities: vec!["a".into()],
            position: 100,
            ..Default::default()
        };
        let resp = build_response(
            Protocol::Hls,
            &state,
            None,
            None,
            &OverrideState::default(),
            &PolicyConfig::default(),
            "/steer",
            &[],
        )
        .unwrap();

        let ss_param = resp.reload_uri.unwrap().split("_ss=").nth(1).unwrap().to_string();
        let decoded = crate::state::decode_state(&ss_param).unwrap();
        assert_eq!(decoded.position, 100 + 300); // 300 = DEFAULT_TTL
    }

    #[test]
    fn position_saturates_at_max() {
        let state = SessionState {
            priorities: vec!["a".into()],
            position: u64::MAX - 10,
            ..Default::default()
        };
        let resp = build_response(
            Protocol::Hls,
            &state,
            None,
            None,
            &OverrideState::default(),
            &PolicyConfig::default(),
            "/steer",
            &[],
        )
        .unwrap();

        let ss_param = resp.reload_uri.unwrap().split("_ss=").nth(1).unwrap().to_string();
        let decoded = crate::state::decode_state(&ss_param).unwrap();
        assert_eq!(decoded.position, u64::MAX);
    }

    #[test]
    fn override_generation_tracked_in_state() {
        let state = SessionState {
            priorities: vec!["a".into(), "b".into()],
            override_gen: 0,
            ..Default::default()
        };
        let overrides = OverrideState {
            priority_override: Some(PriorityOverride {
                priorities: vec!["b".into(), "a".into()],
                generation: 7,
                ttl_override: None,
            }),
            generation: 7,
            ..Default::default()
        };
        let resp = build_response(
            Protocol::Hls,
            &state,
            None,
            None,
            &overrides,
            &PolicyConfig::default(),
            "/steer",
            &[],
        )
        .unwrap();

        let ss_param = resp.reload_uri.unwrap().split("_ss=").nth(1).unwrap().to_string();
        let decoded = crate::state::decode_state(&ss_param).unwrap();
        assert_eq!(decoded.override_gen, 7);
    }

    #[test]
    fn priorities_in_state_match_response() {
        let state = SessionState {
            priorities: vec!["a".into(), "b".into(), "c".into()],
            ..Default::default()
        };
        let resp = build_response(
            Protocol::Hls,
            &state,
            None,
            None,
            &OverrideState::default(),
            &PolicyConfig::default(),
            "/steer",
            &[],
        )
        .unwrap();

        let ss_param = resp.reload_uri.unwrap().split("_ss=").nth(1).unwrap().to_string();
        let decoded = crate::state::decode_state(&ss_param).unwrap();
        assert_eq!(decoded.priorities, resp.pathway_priority.unwrap());
    }

    // ─── Passthrough params ─────────────────────────────────────────────

    #[test]
    fn akamai_tokens_preserved_in_reload_uri() {
        let state = SessionState {
            priorities: vec!["cdn-a".into()],
            ..Default::default()
        };
        let tokens = vec![
            ("start".into(), "1772770805".into()),
            ("end".into(), "1772857805".into()),
            ("userId".into(), "93334984".into()),
            ("hashParam".into(), "a7614ed13747de0802fdd8ff5cd440b4".into()),
        ];
        let resp = build_response(
            Protocol::Hls,
            &state,
            Some("cdn-a"),
            Some(5_000_000),
            &OverrideState::default(),
            &PolicyConfig::default(),
            "/steer",
            &tokens,
        )
        .unwrap();

        let uri = resp.reload_uri.unwrap();
        assert!(uri.contains("start=1772770805"));
        assert!(uri.contains("end=1772857805"));
        assert!(uri.contains("userId=93334984"));
        assert!(uri.contains("hashParam=a7614ed13747de0802fdd8ff5cd440b4"));
    }

    #[test]
    fn no_throughput_no_map_update() {
        let state = SessionState {
            priorities: vec!["a".into()],
            throughput_map: vec![("a".into(), 1_000_000)],
            ..Default::default()
        };
        let resp = build_response(
            Protocol::Hls,
            &state,
            None,
            None,
            &OverrideState::default(),
            &PolicyConfig::default(),
            "/steer",
            &[],
        )
        .unwrap();

        let ss_param = resp.reload_uri.unwrap().split("_ss=").nth(1).unwrap().to_string();
        let decoded = crate::state::decode_state(&ss_param).unwrap();
        // Existing throughput data preserved, no new entry
        assert_eq!(decoded.throughput_map.len(), 1);
        assert_eq!(decoded.throughput_map[0].1, 1_000_000);
    }

    // ─── Master override persisted in RELOAD-URI state ─────────────────

    #[test]
    fn override_priorities_persisted_in_reload_uri_state() {
        // Client state has ["cdn-a", "cdn-b"] but master override says ["cdn-b", "cdn-a"].
        // The state encoded in RELOAD-URI must carry the override priorities,
        // NOT the client's original priorities.
        let state = SessionState {
            priorities: vec!["cdn-a".into(), "cdn-b".into()],
            override_gen: 0,
            ..Default::default()
        };
        let overrides = OverrideState {
            priority_override: Some(PriorityOverride {
                priorities: vec!["cdn-b".into(), "cdn-a".into()],
                generation: 1,
                ttl_override: None,
            }),
            generation: 1,
            ..Default::default()
        };
        let resp = build_response(
            Protocol::Hls,
            &state,
            Some("cdn-a"),
            Some(5_000_000),
            &overrides,
            &PolicyConfig::default(),
            "/steer",
            &[],
        )
        .unwrap();

        // Decode state from RELOAD-URI
        let ss_param = resp.reload_uri.unwrap().split("_ss=").nth(1).unwrap().to_string();
        let decoded = crate::state::decode_state(&ss_param).unwrap();

        // State must reflect master's priorities, not the client's original
        assert_eq!(decoded.priorities, vec!["cdn-b", "cdn-a"]);
        // Override generation must be tracked
        assert_eq!(decoded.override_gen, 1);
    }

    #[test]
    fn newer_override_updates_state_priorities_and_gen() {
        // Client state came from a previous override (gen=1, priorities=["cdn-b","cdn-a"]).
        // Master has pushed a newer override (gen=2, priorities=["cdn-c","cdn-a"]).
        // RELOAD-URI state must reflect the NEW override.
        let state = SessionState {
            priorities: vec!["cdn-b".into(), "cdn-a".into()],
            override_gen: 1,
            ..Default::default()
        };
        let overrides = OverrideState {
            priority_override: Some(PriorityOverride {
                priorities: vec!["cdn-c".into(), "cdn-a".into()],
                generation: 2,
                ttl_override: None,
            }),
            generation: 2,
            ..Default::default()
        };
        let resp = build_response(
            Protocol::Hls,
            &state,
            None,
            None,
            &overrides,
            &PolicyConfig::default(),
            "/steer",
            &[],
        )
        .unwrap();

        let ss_param = resp.reload_uri.unwrap().split("_ss=").nth(1).unwrap().to_string();
        let decoded = crate::state::decode_state(&ss_param).unwrap();

        assert_eq!(decoded.priorities, vec!["cdn-c", "cdn-a"]);
        assert_eq!(decoded.override_gen, 2);
    }

    // ─── JSON serialization ─────────────────────────────────────────────

    #[test]
    fn hls_response_json_format() {
        let state = SessionState {
            priorities: vec!["CDN-A".into(), "CDN-B".into()],
            ..Default::default()
        };
        let resp = build_response(
            Protocol::Hls,
            &state,
            None,
            None,
            &OverrideState::default(),
            &PolicyConfig::default(),
            "/steer",
            &[],
        )
        .unwrap();

        let json = serde_json::to_string(&resp).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["VERSION"], 1);
        assert_eq!(parsed["TTL"], 300);
        assert!(parsed["RELOAD-URI"].is_string());
        assert!(parsed["PATHWAY-PRIORITY"].is_array());
        assert!(parsed.get("SERVICE-LOCATION-PRIORITY").is_none());
    }

    #[test]
    fn dash_response_json_format() {
        let state = SessionState {
            priorities: vec!["alpha".into(), "beta".into()],
            ..Default::default()
        };
        let resp = build_response(
            Protocol::Dash,
            &state,
            None,
            None,
            &OverrideState::default(),
            &PolicyConfig::default(),
            "/steer",
            &[],
        )
        .unwrap();

        let json = serde_json::to_string(&resp).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["VERSION"], 1);
        assert!(parsed["SERVICE-LOCATION-PRIORITY"].is_array());
        assert!(parsed.get("PATHWAY-PRIORITY").is_none());
    }
}
