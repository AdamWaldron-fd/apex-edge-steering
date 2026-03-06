mod control;
mod policy;
mod response;
mod state;
pub mod types;

use std::cell::RefCell;
use wasm_bindgen::prelude::*;

use types::{ControlCommand, OverrideState, Protocol, SessionState, SteeringRequest};

// ─── Initial State Storage ──────────────────────────────────────────────────

thread_local! {
    /// Initial session state set by the master steering server via encode_initial_state.
    /// Used as fallback when a client request has no _ss parameter.
    static INITIAL_STATE: RefCell<Option<SessionState>> = RefCell::new(None);
}

// ─── WASM Exports ────────────────────────────────────────────────────────────

/// Process a steering request and return a JSON steering response.
///
/// This is the main WASM entry point called by all platform wrappers.
///
/// # Arguments
/// * `request_json` - JSON-serialized `SteeringRequest`
/// * `overrides_json` - JSON-serialized `OverrideState` (current edge overrides)
/// * `config_json` - JSON-serialized `PolicyConfig` (optional, uses defaults if empty)
/// * `base_path` - The base path for RELOAD-URI construction (e.g., "/steer")
///
/// # Returns
/// JSON string of the `SteeringResponse` to send back to the player.
#[wasm_bindgen]
pub fn handle_steering_request(
    request_json: &str,
    overrides_json: &str,
    config_json: &str,
    base_path: &str,
) -> Result<String, JsError> {
    let request: SteeringRequest =
        serde_json::from_str(request_json).map_err(|e| JsError::new(&format!("bad request: {e}")))?;

    let overrides: OverrideState = if overrides_json.is_empty() {
        OverrideState::default()
    } else {
        serde_json::from_str(overrides_json)
            .map_err(|e| JsError::new(&format!("bad overrides: {e}")))?
    };

    let config: policy::PolicyConfig = if config_json.is_empty() {
        policy::PolicyConfig::default()
    } else {
        serde_json::from_str(config_json)
            .map_err(|e| JsError::new(&format!("bad config: {e}")))?
    };

    // Use session state from request (_ss param). If absent, fall back to the
    // initial state set by the master steering server via encode_initial_state.
    let session_state = request.session_state.unwrap_or_else(|| {
        INITIAL_STATE.with(|cell| cell.borrow().clone()).unwrap_or_default()
    });

    let passthrough: Vec<(String, String)> = parse_passthrough(&request.raw_query);

    let resp = response::build_response(
        request.protocol,
        &session_state,
        request.pathway.as_deref(),
        request.throughput,
        &overrides,
        &config,
        base_path,
        &passthrough,
    )
    .map_err(|e| JsError::new(&e))?;

    serde_json::to_string(&resp).map_err(|e| JsError::new(&format!("serialize response: {e}")))
}

/// Parse a raw query string into a `SteeringRequest` JSON.
/// Convenience function for platform wrappers that receive raw HTTP query strings.
#[wasm_bindgen]
pub fn parse_request(query_string: &str, protocol_hint: &str) -> Result<String, JsError> {
    let parsed = state::parse_query(query_string);

    let protocol = parsed.protocol.unwrap_or(match protocol_hint {
        "hls" | "HLS" => Protocol::Hls,
        "dash" | "DASH" => Protocol::Dash,
        _ => Protocol::Hls,
    });

    let session_state = match parsed.session_state_raw {
        Some(ref encoded) => Some(
            state::decode_state(encoded)
                .map_err(|e| JsError::new(&format!("bad session state: {e}")))?,
        ),
        None => None,
    };

    let request = SteeringRequest {
        protocol,
        pathway: parsed.pathway,
        throughput: parsed.throughput,
        session_state,
        raw_query: query_string.to_string(),
    };

    serde_json::to_string(&request).map_err(|e| JsError::new(&format!("serialize: {e}")))
}

/// Apply a control command from the master server.
/// Takes current overrides JSON and a command JSON, returns updated overrides JSON.
#[wasm_bindgen]
pub fn apply_control_command(
    overrides_json: &str,
    command_json: &str,
) -> Result<String, JsError> {
    let mut overrides: OverrideState = if overrides_json.is_empty() {
        OverrideState::default()
    } else {
        serde_json::from_str(overrides_json)
            .map_err(|e| JsError::new(&format!("bad overrides: {e}")))?
    };

    let cmd: ControlCommand = serde_json::from_str(command_json)
        .map_err(|e| JsError::new(&format!("bad command: {e}")))?;

    control::apply_command(&mut overrides, &cmd);

    serde_json::to_string(&overrides).map_err(|e| JsError::new(&format!("serialize: {e}")))
}

/// Encode a session state into a base64 string for embedding in manifests.
/// Used by the master steering server to set initial state on the edge server.
///
/// This function both:
/// 1. Returns the base64-encoded state string (for embedding in SERVER-URI)
/// 2. Stores the state on the edge server as fallback for requests without `_ss`
#[wasm_bindgen]
pub fn encode_initial_state(state_json: &str) -> Result<String, JsError> {
    let state: SessionState = serde_json::from_str(state_json)
        .map_err(|e| JsError::new(&format!("bad state: {e}")))?;
    INITIAL_STATE.with(|cell| {
        *cell.borrow_mut() = Some(state.clone());
    });
    state::encode_state(&state).map_err(|e| JsError::new(&e))
}

/// Clear the stored initial state. Used by platform wrappers for reset operations.
#[wasm_bindgen]
pub fn reset_initial_state() {
    INITIAL_STATE.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

// ─── Internal Helpers ────────────────────────────────────────────────────────

/// Extract passthrough query parameters (everything that isn't _HLS_*, _DASH_*, or _ss).
fn parse_passthrough(query: &str) -> Vec<(String, String)> {
    query
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
        .collect()
}

// Re-export for Rust consumers (non-WASM).
pub use control::apply_command;
pub use policy::{evaluate, PolicyConfig};
pub use response::build_response;
pub use state::{decode_state, encode_state, parse_query};

/// Set the initial state for Rust consumers (non-WASM equivalent of encode_initial_state storage).
pub fn set_initial_state(state: &SessionState) {
    INITIAL_STATE.with(|cell| {
        *cell.borrow_mut() = Some(state.clone());
    });
}

/// Clear the initial state for Rust consumers (non-WASM equivalent of reset_initial_state).
pub fn clear_initial_state() {
    INITIAL_STATE.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

/// Get the stored initial state (for testing).
pub fn get_initial_state() -> Option<SessionState> {
    INITIAL_STATE.with(|cell| cell.borrow().clone())
}
