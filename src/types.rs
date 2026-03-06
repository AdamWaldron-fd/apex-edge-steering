use serde::{Deserialize, Serialize};

// ─── Protocol Detection ─────────────────────────────────────────────────────

/// Which streaming protocol the client is using.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Hls,
    Dash,
}

// ─── Client Request ─────────────────────────────────────────────────────────

/// Parsed parameters from an incoming steering request.
/// Combines both HLS and DASH query parameter formats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SteeringRequest {
    /// Detected protocol (HLS or DASH).
    pub protocol: Protocol,

    /// Current pathway/service-location in use by the client.
    /// HLS: `_HLS_pathway`, DASH: `_DASH_pathway`.
    pub pathway: Option<String>,

    /// Client-measured throughput in bits/sec.
    /// HLS: `_HLS_throughput`, DASH: `_DASH_throughput`.
    pub throughput: Option<u64>,

    /// Opaque session state recovered from the RELOAD-URI.
    /// Contains encoded `SessionState` from the previous response.
    pub session_state: Option<SessionState>,

    /// Raw query string (for passing through custom parameters).
    pub raw_query: String,
}

// ─── Session State (carried in URL parameters) ──────────────────────────────

/// All state variables that the edge server needs to persist across requests.
/// This is encoded into the RELOAD-URI so the server remains stateless.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionState {
    /// Pathway priority list as set by the steering master.
    pub priorities: Vec<String>,

    /// Per-pathway throughput observations (pathway_id → bits/sec).
    /// Aggregated from client reports and regional CDN stats.
    #[serde(default)]
    pub throughput_map: Vec<(String, u64)>,

    /// Minimum bitrate in the encoding ladder (bits/sec).
    #[serde(default)]
    pub min_bitrate: u64,

    /// Maximum bitrate in the encoding ladder (bits/sec).
    #[serde(default)]
    pub max_bitrate: u64,

    /// Media duration in seconds (0 = live/unknown).
    #[serde(default)]
    pub duration: u64,

    /// Approximate playback position in seconds.
    #[serde(default)]
    pub position: u64,

    /// Epoch timestamp (seconds) when this state was created.
    #[serde(default)]
    pub timestamp: u64,

    /// Override generation — incremented when master forces an update.
    /// Edge servers use this to detect stale vs fresh overrides.
    #[serde(default)]
    pub override_gen: u64,
}

// ─── Steering Response ──────────────────────────────────────────────────────

/// The JSON body returned to the player.
/// Compatible with both HLS and DASH steering manifest formats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SteeringResponse {
    /// Must be 1 per both specs.
    #[serde(rename = "VERSION")]
    pub version: u32,

    /// Seconds until the client should reload.
    #[serde(rename = "TTL")]
    pub ttl: u32,

    /// URI for the client's next request. Contains encoded session state.
    #[serde(rename = "RELOAD-URI", skip_serializing_if = "Option::is_none")]
    pub reload_uri: Option<String>,

    /// HLS: Ordered pathway preference list.
    #[serde(
        rename = "PATHWAY-PRIORITY",
        skip_serializing_if = "Option::is_none"
    )]
    pub pathway_priority: Option<Vec<String>>,

    /// DASH: Ordered service-location preference list.
    #[serde(
        rename = "SERVICE-LOCATION-PRIORITY",
        skip_serializing_if = "Option::is_none"
    )]
    pub service_location_priority: Option<Vec<String>>,
}

impl SteeringResponse {
    /// Create a new response with VERSION=1 and default 300s TTL.
    pub fn new(protocol: Protocol, priorities: Vec<String>, ttl: u32) -> Self {
        let (pathway_priority, service_location_priority) = match protocol {
            Protocol::Hls => (Some(priorities), None),
            Protocol::Dash => (None, Some(priorities)),
        };
        Self {
            version: 1,
            ttl,
            reload_uri: None,
            pathway_priority,
            service_location_priority,
        }
    }
}

// ─── Control Plane (Master → Edge) ──────────────────────────────────────────

/// A command pushed from the master steering server to edge servers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ControlCommand {
    /// Force a specific CDN priority order for a region or globally.
    #[serde(rename = "set_priorities")]
    SetPriorities {
        /// Optional region filter (e.g. "us-east", "eu-west"). None = global.
        region: Option<String>,
        /// New priority order.
        priorities: Vec<String>,
        /// Override generation (monotonically increasing).
        generation: u64,
        /// TTL override in seconds (None = use default).
        ttl_override: Option<u32>,
    },

    /// Exclude a specific pathway/CDN (disaster recovery, maintenance).
    #[serde(rename = "exclude_pathway")]
    ExcludePathway {
        region: Option<String>,
        pathway: String,
        generation: u64,
    },

    /// Clear all overrides, revert to master-assigned defaults.
    #[serde(rename = "clear_overrides")]
    ClearOverrides {
        region: Option<String>,
        generation: u64,
    },
}

/// Current override state held in edge server memory.
/// This is the only mutable state — updated via control plane pushes.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OverrideState {
    /// Active priority override (if any).
    pub priority_override: Option<PriorityOverride>,
    /// Excluded pathways.
    pub excluded_pathways: Vec<String>,
    /// Current generation number.
    pub generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorityOverride {
    pub priorities: Vec<String>,
    pub generation: u64,
    pub ttl_override: Option<u32>,
}
