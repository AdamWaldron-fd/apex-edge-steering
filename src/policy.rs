use serde::{Deserialize, Serialize};

use crate::types::{OverrideState, Protocol, SessionState, SteeringResponse};

/// Default TTL in seconds (5 minutes per spec recommendation).
pub const DEFAULT_TTL: u32 = 300;

/// Minimum TTL for QoE-optimized sessions (short enough to detect CDN degradation).
pub const QOE_TTL: u32 = 10;

/// Configuration for the policy engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyConfig {
    /// Default TTL when no overrides are active.
    pub default_ttl: u32,

    /// TTL to use during active QoE optimization (short polling).
    pub qoe_ttl: u32,

    /// Throughput threshold below which a pathway is considered degraded.
    /// Expressed as a fraction of the minimum bitrate in the ladder.
    /// e.g., 1.2 means "degraded if throughput < 1.2 × min_bitrate".
    pub degradation_factor: f64,

    /// Whether to enable QoE-based CDN switching.
    pub qoe_enabled: bool,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            default_ttl: DEFAULT_TTL,
            qoe_ttl: QOE_TTL,
            degradation_factor: 1.2,
            qoe_enabled: true,
        }
    }
}

/// Core policy decision: given the current state, overrides, and client report,
/// produce a steering response with the correct CDN priority order and TTL.
pub fn evaluate(
    protocol: Protocol,
    state: &SessionState,
    client_pathway: Option<&str>,
    client_throughput: Option<u64>,
    overrides: &OverrideState,
    config: &PolicyConfig,
) -> SteeringResponse {
    // Start with master-assigned priorities from session state.
    let mut priorities = state.priorities.clone();

    // Apply master-forced priority override if present and newer.
    if let Some(ref ov) = overrides.priority_override {
        if ov.generation >= state.override_gen {
            priorities = ov.priorities.clone();
        }
    }

    // Remove excluded pathways.
    if !overrides.excluded_pathways.is_empty() {
        priorities.retain(|p| !overrides.excluded_pathways.contains(p));
    }

    // Determine TTL.
    let mut ttl = overrides
        .priority_override
        .as_ref()
        .and_then(|ov| ov.ttl_override)
        .unwrap_or(config.default_ttl);

    // QoE optimization: if the client reports low throughput on the current
    // pathway, promote the next pathway and reduce TTL for faster re-evaluation.
    if config.qoe_enabled {
        if let (Some(pathway), Some(throughput)) = (client_pathway, client_throughput) {
            let degraded = if state.min_bitrate > 0 {
                (throughput as f64) < (state.min_bitrate as f64 * config.degradation_factor)
            } else {
                false
            };

            if degraded {
                // Move the degraded pathway down in the priority list.
                if let Some(pos) = priorities.iter().position(|p| p == pathway) {
                    if pos == 0 && priorities.len() > 1 {
                        priorities.swap(0, 1);
                        ttl = config.qoe_ttl;
                    }
                }
            }
        }
    }

    // Ensure we have at least one pathway.
    if priorities.is_empty() {
        priorities = state.priorities.clone();
    }

    SteeringResponse::new(protocol, priorities, ttl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PriorityOverride;

    fn make_state() -> SessionState {
        SessionState {
            priorities: vec!["alpha".into(), "beta".into(), "gamma".into()],
            min_bitrate: 1_000_000,
            max_bitrate: 8_000_000,
            ..Default::default()
        }
    }

    // ─── Basic priority pass-through ─────────────────────────────────────

    #[test]
    fn default_priorities_from_state() {
        let state = make_state();
        let resp = evaluate(
            Protocol::Hls,
            &state,
            None,
            None,
            &OverrideState::default(),
            &PolicyConfig::default(),
        );
        assert_eq!(
            resp.pathway_priority,
            Some(vec!["alpha".into(), "beta".into(), "gamma".into()])
        );
        assert_eq!(resp.ttl, DEFAULT_TTL);
        assert_eq!(resp.version, 1);
    }

    #[test]
    fn single_pathway() {
        let state = SessionState {
            priorities: vec!["only-cdn".into()],
            ..Default::default()
        };
        let resp = evaluate(
            Protocol::Hls,
            &state,
            Some("only-cdn"),
            Some(5_000_000),
            &OverrideState::default(),
            &PolicyConfig::default(),
        );
        assert_eq!(
            resp.pathway_priority,
            Some(vec!["only-cdn".into()])
        );
    }

    #[test]
    fn empty_priorities_preserved() {
        let state = SessionState::default();
        let resp = evaluate(
            Protocol::Hls,
            &state,
            None,
            None,
            &OverrideState::default(),
            &PolicyConfig::default(),
        );
        // Empty state priorities → empty response priorities
        assert_eq!(resp.pathway_priority, Some(vec![]));
    }

    // ─── HLS vs DASH response format ────────────────────────────────────

    #[test]
    fn hls_uses_pathway_priority() {
        let state = make_state();
        let resp = evaluate(
            Protocol::Hls,
            &state,
            None,
            None,
            &OverrideState::default(),
            &PolicyConfig::default(),
        );
        assert!(resp.pathway_priority.is_some());
        assert!(resp.service_location_priority.is_none());
    }

    #[test]
    fn dash_uses_service_location_priority() {
        let state = make_state();
        let resp = evaluate(
            Protocol::Dash,
            &state,
            None,
            None,
            &OverrideState::default(),
            &PolicyConfig::default(),
        );
        assert!(resp.pathway_priority.is_none());
        assert!(resp.service_location_priority.is_some());
        assert_eq!(
            resp.service_location_priority.unwrap(),
            vec!["alpha", "beta", "gamma"]
        );
    }

    // ─── Master override tests ──────────────────────────────────────────

    #[test]
    fn master_override_replaces_priorities() {
        let state = make_state();
        let overrides = OverrideState {
            priority_override: Some(PriorityOverride {
                priorities: vec!["gamma".into(), "alpha".into()],
                generation: 1,
                ttl_override: Some(60),
            }),
            generation: 1,
            ..Default::default()
        };
        let resp = evaluate(
            Protocol::Dash,
            &state,
            None,
            None,
            &overrides,
            &PolicyConfig::default(),
        );
        assert_eq!(
            resp.service_location_priority,
            Some(vec!["gamma".into(), "alpha".into()])
        );
        assert_eq!(resp.ttl, 60);
    }

    #[test]
    fn stale_override_ignored_when_state_has_newer_gen() {
        let mut state = make_state();
        state.override_gen = 5;
        let overrides = OverrideState {
            priority_override: Some(PriorityOverride {
                priorities: vec!["gamma".into()],
                generation: 3, // Older than state's override_gen
                ttl_override: None,
            }),
            generation: 3,
            ..Default::default()
        };
        let resp = evaluate(
            Protocol::Hls,
            &state,
            None,
            None,
            &overrides,
            &PolicyConfig::default(),
        );
        // Should use state priorities, not override
        assert_eq!(resp.pathway_priority.unwrap()[0], "alpha");
    }

    #[test]
    fn override_with_equal_generation_applied() {
        let mut state = make_state();
        state.override_gen = 3;
        let overrides = OverrideState {
            priority_override: Some(PriorityOverride {
                priorities: vec!["gamma".into()],
                generation: 3, // Equal to state's override_gen
                ttl_override: None,
            }),
            generation: 3,
            ..Default::default()
        };
        let resp = evaluate(
            Protocol::Hls,
            &state,
            None,
            None,
            &overrides,
            &PolicyConfig::default(),
        );
        assert_eq!(resp.pathway_priority.unwrap()[0], "gamma");
    }

    #[test]
    fn override_ttl_used_when_present() {
        let state = make_state();
        let overrides = OverrideState {
            priority_override: Some(PriorityOverride {
                priorities: vec!["alpha".into()],
                generation: 1,
                ttl_override: Some(15),
            }),
            generation: 1,
            ..Default::default()
        };
        let resp = evaluate(
            Protocol::Hls,
            &state,
            None,
            None,
            &overrides,
            &PolicyConfig::default(),
        );
        assert_eq!(resp.ttl, 15);
    }

    #[test]
    fn override_without_ttl_uses_config_default() {
        let state = make_state();
        let overrides = OverrideState {
            priority_override: Some(PriorityOverride {
                priorities: vec!["alpha".into()],
                generation: 1,
                ttl_override: None,
            }),
            generation: 1,
            ..Default::default()
        };
        let resp = evaluate(
            Protocol::Hls,
            &state,
            None,
            None,
            &overrides,
            &PolicyConfig::default(),
        );
        assert_eq!(resp.ttl, DEFAULT_TTL);
    }

    // ─── Pathway exclusion tests ────────────────────────────────────────

    #[test]
    fn excluded_pathways_removed() {
        let state = make_state();
        let overrides = OverrideState {
            excluded_pathways: vec!["beta".into()],
            ..Default::default()
        };
        let resp = evaluate(
            Protocol::Hls,
            &state,
            None,
            None,
            &overrides,
            &PolicyConfig::default(),
        );
        assert_eq!(
            resp.pathway_priority,
            Some(vec!["alpha".into(), "gamma".into()])
        );
    }

    #[test]
    fn exclude_multiple_pathways() {
        let state = make_state();
        let overrides = OverrideState {
            excluded_pathways: vec!["alpha".into(), "gamma".into()],
            ..Default::default()
        };
        let resp = evaluate(
            Protocol::Hls,
            &state,
            None,
            None,
            &overrides,
            &PolicyConfig::default(),
        );
        assert_eq!(resp.pathway_priority, Some(vec!["beta".into()]));
    }

    #[test]
    fn exclude_all_pathways_falls_back_to_state() {
        let state = make_state();
        let overrides = OverrideState {
            excluded_pathways: vec!["alpha".into(), "beta".into(), "gamma".into()],
            ..Default::default()
        };
        let resp = evaluate(
            Protocol::Hls,
            &state,
            None,
            None,
            &overrides,
            &PolicyConfig::default(),
        );
        // All excluded → fallback to original state priorities
        assert_eq!(
            resp.pathway_priority,
            Some(vec!["alpha".into(), "beta".into(), "gamma".into()])
        );
    }

    #[test]
    fn exclude_nonexistent_pathway_is_noop() {
        let state = make_state();
        let overrides = OverrideState {
            excluded_pathways: vec!["nonexistent".into()],
            ..Default::default()
        };
        let resp = evaluate(
            Protocol::Hls,
            &state,
            None,
            None,
            &overrides,
            &PolicyConfig::default(),
        );
        assert_eq!(
            resp.pathway_priority,
            Some(vec!["alpha".into(), "beta".into(), "gamma".into()])
        );
    }

    #[test]
    fn exclude_combined_with_override() {
        let state = make_state();
        let overrides = OverrideState {
            priority_override: Some(PriorityOverride {
                priorities: vec!["gamma".into(), "beta".into(), "alpha".into()],
                generation: 1,
                ttl_override: None,
            }),
            excluded_pathways: vec!["beta".into()],
            generation: 1,
        };
        let resp = evaluate(
            Protocol::Hls,
            &state,
            None,
            None,
            &overrides,
            &PolicyConfig::default(),
        );
        // Override priorities with beta excluded
        assert_eq!(
            resp.pathway_priority,
            Some(vec!["gamma".into(), "alpha".into()])
        );
    }

    // ─── QoE optimization tests ─────────────────────────────────────────

    #[test]
    fn qoe_demotes_degraded_pathway() {
        let state = make_state();
        let resp = evaluate(
            Protocol::Hls,
            &state,
            Some("alpha"),
            Some(500_000), // Below 1.2 × 1_000_000 = 1_200_000
            &OverrideState::default(),
            &PolicyConfig::default(),
        );
        assert_eq!(resp.pathway_priority.as_ref().unwrap()[0], "beta");
        assert_eq!(resp.pathway_priority.as_ref().unwrap()[1], "alpha");
        assert_eq!(resp.ttl, QOE_TTL);
    }

    #[test]
    fn qoe_no_action_when_throughput_ok() {
        let state = make_state();
        let resp = evaluate(
            Protocol::Hls,
            &state,
            Some("alpha"),
            Some(5_000_000),
            &OverrideState::default(),
            &PolicyConfig::default(),
        );
        assert_eq!(resp.pathway_priority.as_ref().unwrap()[0], "alpha");
        assert_eq!(resp.ttl, DEFAULT_TTL);
    }

    #[test]
    fn qoe_exactly_at_threshold_not_degraded() {
        let state = make_state();
        // Threshold is 1.2 × 1_000_000 = 1_200_000. At exactly the threshold,
        // throughput < threshold is false.
        let resp = evaluate(
            Protocol::Hls,
            &state,
            Some("alpha"),
            Some(1_200_000),
            &OverrideState::default(),
            &PolicyConfig::default(),
        );
        assert_eq!(resp.pathway_priority.as_ref().unwrap()[0], "alpha");
        assert_eq!(resp.ttl, DEFAULT_TTL);
    }

    #[test]
    fn qoe_just_below_threshold_is_degraded() {
        let state = make_state();
        let resp = evaluate(
            Protocol::Hls,
            &state,
            Some("alpha"),
            Some(1_199_999),
            &OverrideState::default(),
            &PolicyConfig::default(),
        );
        assert_eq!(resp.pathway_priority.as_ref().unwrap()[0], "beta");
        assert_eq!(resp.ttl, QOE_TTL);
    }

    #[test]
    fn qoe_disabled_no_demotion() {
        let state = make_state();
        let config = PolicyConfig {
            qoe_enabled: false,
            ..PolicyConfig::default()
        };
        let resp = evaluate(
            Protocol::Hls,
            &state,
            Some("alpha"),
            Some(100), // Extremely low throughput
            &OverrideState::default(),
            &config,
        );
        // QoE disabled: no demotion should occur
        assert_eq!(resp.pathway_priority.as_ref().unwrap()[0], "alpha");
        assert_eq!(resp.ttl, DEFAULT_TTL);
    }

    #[test]
    fn qoe_no_demotion_when_min_bitrate_zero() {
        let mut state = make_state();
        state.min_bitrate = 0;
        let resp = evaluate(
            Protocol::Hls,
            &state,
            Some("alpha"),
            Some(100),
            &OverrideState::default(),
            &PolicyConfig::default(),
        );
        // min_bitrate=0 means we can't determine degradation
        assert_eq!(resp.pathway_priority.as_ref().unwrap()[0], "alpha");
    }

    #[test]
    fn qoe_no_demotion_for_non_top_pathway() {
        let state = make_state();
        // Client reports low throughput on "beta" which is 2nd in priority.
        // Only the top pathway should be demoted.
        let resp = evaluate(
            Protocol::Hls,
            &state,
            Some("beta"),
            Some(100),
            &OverrideState::default(),
            &PolicyConfig::default(),
        );
        // beta is not at position 0, so no swap
        assert_eq!(resp.pathway_priority.as_ref().unwrap()[0], "alpha");
        assert_eq!(resp.ttl, DEFAULT_TTL);
    }

    #[test]
    fn qoe_no_demotion_single_pathway() {
        let state = SessionState {
            priorities: vec!["only-one".into()],
            min_bitrate: 1_000_000,
            ..Default::default()
        };
        let resp = evaluate(
            Protocol::Hls,
            &state,
            Some("only-one"),
            Some(100),
            &OverrideState::default(),
            &PolicyConfig::default(),
        );
        // Single pathway: can't demote, nowhere to go
        assert_eq!(resp.pathway_priority.unwrap(), vec!["only-one"]);
    }

    #[test]
    fn qoe_no_demotion_unknown_pathway() {
        let state = make_state();
        let resp = evaluate(
            Protocol::Hls,
            &state,
            Some("unknown-cdn"),
            Some(100),
            &OverrideState::default(),
            &PolicyConfig::default(),
        );
        // Unknown pathway not in list: no action
        assert_eq!(resp.pathway_priority.as_ref().unwrap()[0], "alpha");
    }

    #[test]
    fn qoe_custom_degradation_factor() {
        let state = make_state();
        let config = PolicyConfig {
            degradation_factor: 2.0, // Very aggressive: degrade if < 2×min
            ..PolicyConfig::default()
        };
        // 1_500_000 is above 1.2×1M but below 2.0×1M
        let resp = evaluate(
            Protocol::Hls,
            &state,
            Some("alpha"),
            Some(1_500_000),
            &OverrideState::default(),
            &config,
        );
        assert_eq!(resp.pathway_priority.as_ref().unwrap()[0], "beta");
    }

    #[test]
    fn qoe_custom_ttl() {
        let state = make_state();
        let config = PolicyConfig {
            qoe_ttl: 5,
            ..PolicyConfig::default()
        };
        let resp = evaluate(
            Protocol::Hls,
            &state,
            Some("alpha"),
            Some(100),
            &OverrideState::default(),
            &config,
        );
        assert_eq!(resp.ttl, 5);
    }

    // ─── Master override takes precedence over client state ────────────

    #[test]
    fn master_override_replaces_client_state_priorities() {
        // Client state carries priorities from initial session setup.
        // Master has pushed a different priority order — the master wins.
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
        let resp = evaluate(
            Protocol::Hls,
            &state,
            Some("cdn-a"),
            Some(5_000_000),
            &overrides,
            &PolicyConfig::default(),
        );
        // Master override priorities must take precedence
        assert_eq!(
            resp.pathway_priority.unwrap(),
            vec!["cdn-b", "cdn-a"]
        );
    }

    #[test]
    fn master_override_persists_when_client_state_already_has_override_gen() {
        // Client state was updated by a previous override (override_gen=1).
        // The SAME override (gen=1) is still active — it must still apply.
        let state = SessionState {
            priorities: vec!["cdn-b".into(), "cdn-a".into()],
            override_gen: 1,
            ..Default::default()
        };
        let overrides = OverrideState {
            priority_override: Some(PriorityOverride {
                priorities: vec!["cdn-b".into(), "cdn-a".into()],
                generation: 1,
                ttl_override: Some(30),
            }),
            generation: 1,
            ..Default::default()
        };
        let resp = evaluate(
            Protocol::Hls,
            &state,
            Some("cdn-b"),
            Some(5_000_000),
            &overrides,
            &PolicyConfig::default(),
        );
        assert_eq!(
            resp.pathway_priority.unwrap(),
            vec!["cdn-b", "cdn-a"]
        );
        assert_eq!(resp.ttl, 30);
    }

    #[test]
    fn newer_master_override_replaces_client_state_from_older_override() {
        // Client has state from override gen=1 (priorities: ["cdn-b", "cdn-a"]).
        // Master has pushed a NEWER override gen=2 with different priorities.
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
        let resp = evaluate(
            Protocol::Hls,
            &state,
            None,
            None,
            &overrides,
            &PolicyConfig::default(),
        );
        // Newer override must win over client's stale state
        assert_eq!(
            resp.pathway_priority.unwrap(),
            vec!["cdn-c", "cdn-a"]
        );
    }

    #[test]
    fn master_override_wins_for_dash_protocol() {
        // Same master-override-takes-precedence behavior for DASH.
        let state = SessionState {
            priorities: vec!["alpha".into(), "beta".into()],
            override_gen: 0,
            ..Default::default()
        };
        let overrides = OverrideState {
            priority_override: Some(PriorityOverride {
                priorities: vec!["beta".into(), "alpha".into()],
                generation: 1,
                ttl_override: None,
            }),
            generation: 1,
            ..Default::default()
        };
        let resp = evaluate(
            Protocol::Dash,
            &state,
            Some("alpha"),
            Some(5_000_000),
            &overrides,
            &PolicyConfig::default(),
        );
        assert_eq!(
            resp.service_location_priority.unwrap(),
            vec!["beta", "alpha"]
        );
    }

    // ─── PolicyConfig custom defaults ───────────────────────────────────

    #[test]
    fn custom_default_ttl() {
        let state = make_state();
        let config = PolicyConfig {
            default_ttl: 600,
            ..PolicyConfig::default()
        };
        let resp = evaluate(
            Protocol::Hls,
            &state,
            None,
            None,
            &OverrideState::default(),
            &config,
        );
        assert_eq!(resp.ttl, 600);
    }
}
