use crate::types::{ControlCommand, OverrideState, PriorityOverride};

/// Apply a control command from the master steering server to the edge override state.
///
/// This mutates the in-memory override state. In edge deployments, this state
/// lives in the worker's memory and is populated on startup or via push updates.
pub fn apply_command(state: &mut OverrideState, cmd: &ControlCommand) {
    match cmd {
        ControlCommand::SetPriorities {
            priorities,
            generation,
            ttl_override,
            ..
        } => {
            if *generation > state.generation {
                state.priority_override = Some(PriorityOverride {
                    priorities: priorities.clone(),
                    generation: *generation,
                    ttl_override: *ttl_override,
                });
                state.generation = *generation;
            }
        }
        ControlCommand::ExcludePathway {
            pathway,
            generation,
            ..
        } => {
            if *generation > state.generation {
                if !state.excluded_pathways.contains(pathway) {
                    state.excluded_pathways.push(pathway.clone());
                }
                state.generation = *generation;
            }
        }
        ControlCommand::ClearOverrides { generation, .. } => {
            if *generation > state.generation {
                state.priority_override = None;
                state.excluded_pathways.clear();
                state.generation = *generation;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── SetPriorities ──────────────────────────────────────────────────

    #[test]
    fn set_priorities_from_clean_state() {
        let mut state = OverrideState::default();
        let cmd = ControlCommand::SetPriorities {
            region: None,
            priorities: vec!["gamma".into(), "alpha".into()],
            generation: 1,
            ttl_override: Some(30),
        };
        apply_command(&mut state, &cmd);
        assert!(state.priority_override.is_some());
        let ov = state.priority_override.as_ref().unwrap();
        assert_eq!(ov.priorities, vec!["gamma", "alpha"]);
        assert_eq!(ov.ttl_override, Some(30));
        assert_eq!(state.generation, 1);
    }

    #[test]
    fn set_priorities_without_ttl_override() {
        let mut state = OverrideState::default();
        let cmd = ControlCommand::SetPriorities {
            region: None,
            priorities: vec!["a".into()],
            generation: 1,
            ttl_override: None,
        };
        apply_command(&mut state, &cmd);
        let ov = state.priority_override.as_ref().unwrap();
        assert_eq!(ov.ttl_override, None);
    }

    #[test]
    fn set_priorities_replaces_existing() {
        let mut state = OverrideState {
            priority_override: Some(PriorityOverride {
                priorities: vec!["old".into()],
                generation: 1,
                ttl_override: Some(60),
            }),
            generation: 1,
            ..Default::default()
        };
        let cmd = ControlCommand::SetPriorities {
            region: None,
            priorities: vec!["new-a".into(), "new-b".into()],
            generation: 2,
            ttl_override: Some(15),
        };
        apply_command(&mut state, &cmd);
        let ov = state.priority_override.as_ref().unwrap();
        assert_eq!(ov.priorities, vec!["new-a", "new-b"]);
        assert_eq!(ov.ttl_override, Some(15));
        assert_eq!(state.generation, 2);
    }

    #[test]
    fn set_priorities_with_region() {
        let mut state = OverrideState::default();
        let cmd = ControlCommand::SetPriorities {
            region: Some("us-east".into()),
            priorities: vec!["a".into()],
            generation: 1,
            ttl_override: None,
        };
        apply_command(&mut state, &cmd);
        // Region is stored on the command but not filtered at this level
        assert!(state.priority_override.is_some());
    }

    // ─── Stale command rejection ────────────────────────────────────────

    #[test]
    fn stale_set_priorities_ignored() {
        let mut state = OverrideState {
            generation: 5,
            ..Default::default()
        };
        let cmd = ControlCommand::SetPriorities {
            region: None,
            priorities: vec!["x".into()],
            generation: 3,
            ttl_override: None,
        };
        apply_command(&mut state, &cmd);
        assert!(state.priority_override.is_none());
        assert_eq!(state.generation, 5);
    }

    #[test]
    fn stale_exclude_ignored() {
        let mut state = OverrideState {
            generation: 5,
            ..Default::default()
        };
        let cmd = ControlCommand::ExcludePathway {
            region: None,
            pathway: "x".into(),
            generation: 3,
        };
        apply_command(&mut state, &cmd);
        assert!(state.excluded_pathways.is_empty());
    }

    #[test]
    fn stale_clear_ignored() {
        let mut state = OverrideState {
            priority_override: Some(PriorityOverride {
                priorities: vec!["a".into()],
                generation: 5,
                ttl_override: None,
            }),
            generation: 5,
            ..Default::default()
        };
        let cmd = ControlCommand::ClearOverrides {
            region: None,
            generation: 3,
        };
        apply_command(&mut state, &cmd);
        assert!(state.priority_override.is_some());
    }

    #[test]
    fn equal_generation_rejected() {
        // Commands with the same generation as current should be rejected
        let mut state = OverrideState {
            generation: 5,
            ..Default::default()
        };
        let cmd = ControlCommand::SetPriorities {
            region: None,
            priorities: vec!["x".into()],
            generation: 5,
            ttl_override: None,
        };
        apply_command(&mut state, &cmd);
        assert!(state.priority_override.is_none());
    }

    // ─── ExcludePathway ─────────────────────────────────────────────────

    #[test]
    fn exclude_single_pathway() {
        let mut state = OverrideState::default();
        let cmd = ControlCommand::ExcludePathway {
            region: None,
            pathway: "beta".into(),
            generation: 1,
        };
        apply_command(&mut state, &cmd);
        assert_eq!(state.excluded_pathways, vec!["beta"]);
        assert_eq!(state.generation, 1);
    }

    #[test]
    fn exclude_multiple_pathways_sequentially() {
        let mut state = OverrideState::default();
        apply_command(&mut state, &ControlCommand::ExcludePathway {
            region: None,
            pathway: "alpha".into(),
            generation: 1,
        });
        apply_command(&mut state, &ControlCommand::ExcludePathway {
            region: None,
            pathway: "beta".into(),
            generation: 2,
        });
        assert_eq!(state.excluded_pathways, vec!["alpha", "beta"]);
        assert_eq!(state.generation, 2);
    }

    #[test]
    fn exclude_duplicate_pathway_not_added_twice() {
        let mut state = OverrideState::default();
        apply_command(&mut state, &ControlCommand::ExcludePathway {
            region: None,
            pathway: "alpha".into(),
            generation: 1,
        });
        apply_command(&mut state, &ControlCommand::ExcludePathway {
            region: None,
            pathway: "alpha".into(),
            generation: 2,
        });
        assert_eq!(state.excluded_pathways, vec!["alpha"]);
        assert_eq!(state.generation, 2);
    }

    // ─── ClearOverrides ─────────────────────────────────────────────────

    #[test]
    fn clear_overrides_resets_everything() {
        let mut state = OverrideState {
            priority_override: Some(PriorityOverride {
                priorities: vec!["x".into()],
                generation: 1,
                ttl_override: None,
            }),
            excluded_pathways: vec!["y".into(), "z".into()],
            generation: 1,
        };
        let cmd = ControlCommand::ClearOverrides {
            region: None,
            generation: 2,
        };
        apply_command(&mut state, &cmd);
        assert!(state.priority_override.is_none());
        assert!(state.excluded_pathways.is_empty());
        assert_eq!(state.generation, 2);
    }

    #[test]
    fn clear_overrides_on_empty_state() {
        let mut state = OverrideState::default();
        let cmd = ControlCommand::ClearOverrides {
            region: None,
            generation: 1,
        };
        apply_command(&mut state, &cmd);
        assert!(state.priority_override.is_none());
        assert!(state.excluded_pathways.is_empty());
        assert_eq!(state.generation, 1);
    }

    // ─── Command sequencing ─────────────────────────────────────────────

    #[test]
    fn set_then_exclude_then_clear() {
        let mut state = OverrideState::default();

        // Set priorities
        apply_command(&mut state, &ControlCommand::SetPriorities {
            region: None,
            priorities: vec!["a".into(), "b".into(), "c".into()],
            generation: 1,
            ttl_override: Some(60),
        });
        assert!(state.priority_override.is_some());

        // Exclude a pathway
        apply_command(&mut state, &ControlCommand::ExcludePathway {
            region: None,
            pathway: "b".into(),
            generation: 2,
        });
        assert_eq!(state.excluded_pathways, vec!["b"]);
        assert!(state.priority_override.is_some()); // Still set

        // Clear everything
        apply_command(&mut state, &ControlCommand::ClearOverrides {
            region: None,
            generation: 3,
        });
        assert!(state.priority_override.is_none());
        assert!(state.excluded_pathways.is_empty());
        assert_eq!(state.generation, 3);
    }

    // ─── JSON serialization of commands ─────────────────────────────────

    #[test]
    fn deserialize_set_priorities_json() {
        let json = r#"{
            "type": "set_priorities",
            "region": "us-east",
            "priorities": ["cdn-b", "cdn-a"],
            "generation": 42,
            "ttl_override": 15
        }"#;
        let cmd: ControlCommand = serde_json::from_str(json).unwrap();
        match cmd {
            ControlCommand::SetPriorities { region, priorities, generation, ttl_override } => {
                assert_eq!(region, Some("us-east".into()));
                assert_eq!(priorities, vec!["cdn-b", "cdn-a"]);
                assert_eq!(generation, 42);
                assert_eq!(ttl_override, Some(15));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn deserialize_exclude_pathway_json() {
        let json = r#"{
            "type": "exclude_pathway",
            "region": null,
            "pathway": "cdn-c",
            "generation": 5
        }"#;
        let cmd: ControlCommand = serde_json::from_str(json).unwrap();
        match cmd {
            ControlCommand::ExcludePathway { region, pathway, generation } => {
                assert_eq!(region, None);
                assert_eq!(pathway, "cdn-c");
                assert_eq!(generation, 5);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn deserialize_clear_overrides_json() {
        let json = r#"{
            "type": "clear_overrides",
            "region": null,
            "generation": 10
        }"#;
        let cmd: ControlCommand = serde_json::from_str(json).unwrap();
        match cmd {
            ControlCommand::ClearOverrides { region, generation } => {
                assert_eq!(region, None);
                assert_eq!(generation, 10);
            }
            _ => panic!("wrong variant"),
        }
    }
}
