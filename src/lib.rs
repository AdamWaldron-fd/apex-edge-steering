mod control;
mod policy;
mod response;
mod state;
pub mod types;

use wasm_bindgen::prelude::*;

use types::{ControlCommand, OverrideState, Protocol, SessionState, SteeringRequest};

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

    let session_state = request.session_state.unwrap_or_default();

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
/// Used by the "manifest updater" to set initial state in SERVER-URI.
#[wasm_bindgen]
pub fn encode_initial_state(state_json: &str) -> Result<String, JsError> {
    let state: SessionState = serde_json::from_str(state_json)
        .map_err(|e| JsError::new(&format!("bad state: {e}")))?;
    state::encode_state(&state).map_err(|e| JsError::new(&e))
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
