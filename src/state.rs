use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json;

use crate::types::SessionState;

/// Encode session state into a URL-safe base64 string.
/// This gets embedded in the RELOAD-URI so the edge server can recover
/// full session context on the next request without any server-side storage.
pub fn encode_state(state: &SessionState) -> Result<String, String> {
    let json = serde_json::to_vec(state).map_err(|e| format!("serialize: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(&json))
}

/// Decode session state from a URL-safe base64 string extracted from
/// the `_ss` query parameter.
pub fn decode_state(encoded: &str) -> Result<SessionState, String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|e| format!("base64: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("json: {e}"))
}

/// Parse a steering request's query string and extract:
/// - Protocol-specific parameters (_HLS_* or _DASH_*)
/// - Session state from `_ss` parameter
/// - All other parameters preserved as-is
pub fn parse_query(query: &str) -> ParsedQuery {
    let mut result = ParsedQuery::default();

    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        match key {
            "_HLS_pathway" => {
                result.protocol = Some(crate::types::Protocol::Hls);
                result.pathway = Some(url_decode(value));
            }
            "_HLS_throughput" => {
                result.protocol = Some(crate::types::Protocol::Hls);
                result.throughput = value.parse().ok();
            }
            "_DASH_pathway" => {
                result.protocol = Some(crate::types::Protocol::Dash);
                result.pathway = Some(url_decode(value));
            }
            "_DASH_throughput" => {
                result.protocol = Some(crate::types::Protocol::Dash);
                result.throughput = value.parse().ok();
            }
            "_ss" => {
                result.session_state_raw = Some(url_decode(value));
            }
            _ => {
                result.passthrough_params.push((key.to_string(), value.to_string()));
            }
        }
    }

    result
}

/// Build a RELOAD-URI that encodes the updated session state.
/// Preserves any passthrough query parameters from the original request.
pub fn build_reload_uri(
    base_path: &str,
    state: &SessionState,
    passthrough: &[(String, String)],
) -> Result<String, String> {
    let encoded = encode_state(state)?;
    let mut uri = base_path.to_string();
    uri.push('?');

    // Passthrough params first
    for (i, (k, v)) in passthrough.iter().enumerate() {
        if i > 0 {
            uri.push('&');
        }
        uri.push_str(k);
        uri.push('=');
        uri.push_str(v);
    }

    // Append session state
    if !passthrough.is_empty() {
        uri.push('&');
    }
    uri.push_str("_ss=");
    uri.push_str(&encoded);

    Ok(uri)
}

#[derive(Debug, Default)]
pub struct ParsedQuery {
    pub protocol: Option<crate::types::Protocol>,
    pub pathway: Option<String>,
    pub throughput: Option<u64>,
    pub session_state_raw: Option<String>,
    pub passthrough_params: Vec<(String, String)>,
}

fn url_decode(s: &str) -> String {
    // Minimal percent-decoding for common cases.
    // Handles %20, %22 (double-quote wrappers from DASH spec), and + as space.
    let mut result = String::with_capacity(s.len());
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        match b {
            b'%' => {
                let hi = chars.next().unwrap_or(0);
                let lo = chars.next().unwrap_or(0);
                let val = hex_val(hi) * 16 + hex_val(lo);
                result.push(val as char);
            }
            b'+' => result.push(' '),
            _ => result.push(b as char),
        }
    }
    // Strip surrounding double-quotes (DASH spec sends pathway in quotes)
    if result.starts_with('"') && result.ends_with('"') && result.len() >= 2 {
        result[1..result.len() - 1].to_string()
    } else {
        result
    }
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── encode_state / decode_state ─────────────────────────────────────

    #[test]
    fn roundtrip_state_full() {
        let state = SessionState {
            priorities: vec!["alpha".into(), "beta".into()],
            throughput_map: vec![("alpha".into(), 5_000_000), ("beta".into(), 3_000_000)],
            min_bitrate: 500_000,
            max_bitrate: 8_000_000,
            duration: 3600,
            position: 120,
            timestamp: 1700000000,
            override_gen: 42,
        };
        let encoded = encode_state(&state).unwrap();
        let decoded = decode_state(&encoded).unwrap();
        assert_eq!(decoded.priorities, state.priorities);
        assert_eq!(decoded.throughput_map, state.throughput_map);
        assert_eq!(decoded.min_bitrate, state.min_bitrate);
        assert_eq!(decoded.max_bitrate, state.max_bitrate);
        assert_eq!(decoded.duration, state.duration);
        assert_eq!(decoded.position, state.position);
        assert_eq!(decoded.timestamp, state.timestamp);
        assert_eq!(decoded.override_gen, state.override_gen);
    }

    #[test]
    fn roundtrip_default_state() {
        let state = SessionState::default();
        let encoded = encode_state(&state).unwrap();
        let decoded = decode_state(&encoded).unwrap();
        assert!(decoded.priorities.is_empty());
        assert!(decoded.throughput_map.is_empty());
        assert_eq!(decoded.min_bitrate, 0);
        assert_eq!(decoded.max_bitrate, 0);
        assert_eq!(decoded.duration, 0);
        assert_eq!(decoded.position, 0);
        assert_eq!(decoded.timestamp, 0);
        assert_eq!(decoded.override_gen, 0);
    }

    #[test]
    fn roundtrip_many_pathways() {
        let state = SessionState {
            priorities: (0..20).map(|i| format!("cdn-{i}")).collect(),
            ..Default::default()
        };
        let encoded = encode_state(&state).unwrap();
        let decoded = decode_state(&encoded).unwrap();
        assert_eq!(decoded.priorities.len(), 20);
        assert_eq!(decoded.priorities[19], "cdn-19");
    }

    #[test]
    fn roundtrip_special_characters_in_pathway() {
        let state = SessionState {
            priorities: vec!["CDN_A-1.primary".into(), "CDN_B-2.backup".into()],
            ..Default::default()
        };
        let encoded = encode_state(&state).unwrap();
        let decoded = decode_state(&encoded).unwrap();
        assert_eq!(decoded.priorities[0], "CDN_A-1.primary");
        assert_eq!(decoded.priorities[1], "CDN_B-2.backup");
    }

    #[test]
    fn encoded_state_is_url_safe() {
        let state = SessionState {
            priorities: vec!["alpha".into(), "beta".into()],
            throughput_map: vec![("alpha".into(), u64::MAX)],
            min_bitrate: u64::MAX,
            max_bitrate: u64::MAX,
            ..Default::default()
        };
        let encoded = encode_state(&state).unwrap();
        // URL-safe base64 should not contain +, /, or =
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));
    }

    #[test]
    fn decode_invalid_base64_returns_error() {
        let result = decode_state("not-valid-base64!!!");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("base64"));
    }

    #[test]
    fn decode_valid_base64_invalid_json_returns_error() {
        // Valid base64 encoding of "not json"
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"not json");
        let result = decode_state(&encoded);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("json"));
    }

    #[test]
    fn decode_empty_string_returns_error() {
        let result = decode_state("");
        assert!(result.is_err());
    }

    // ─── parse_query: HLS ────────────────────────────────────────────────

    #[test]
    fn parse_hls_query_full() {
        let q = "_HLS_pathway=CDN-A&_HLS_throughput=5140000&_ss=abc123&session=xyz";
        let parsed = parse_query(q);
        assert_eq!(parsed.protocol, Some(crate::types::Protocol::Hls));
        assert_eq!(parsed.pathway, Some("CDN-A".to_string()));
        assert_eq!(parsed.throughput, Some(5_140_000));
        assert!(parsed.session_state_raw.is_some());
        assert_eq!(parsed.passthrough_params.len(), 1);
        assert_eq!(parsed.passthrough_params[0].0, "session");
        assert_eq!(parsed.passthrough_params[0].1, "xyz");
    }

    #[test]
    fn parse_hls_pathway_only() {
        let q = "_HLS_pathway=CDN-B";
        let parsed = parse_query(q);
        assert_eq!(parsed.protocol, Some(crate::types::Protocol::Hls));
        assert_eq!(parsed.pathway, Some("CDN-B".to_string()));
        assert_eq!(parsed.throughput, None);
        assert!(parsed.session_state_raw.is_none());
    }

    #[test]
    fn parse_hls_throughput_only() {
        let q = "_HLS_throughput=10000000";
        let parsed = parse_query(q);
        assert_eq!(parsed.protocol, Some(crate::types::Protocol::Hls));
        assert_eq!(parsed.pathway, None);
        assert_eq!(parsed.throughput, Some(10_000_000));
    }

    // ─── parse_query: DASH ───────────────────────────────────────────────

    #[test]
    fn parse_dash_query_quoted_pathway() {
        let q = "_DASH_pathway=%22beta%22&_DASH_throughput=4880000";
        let parsed = parse_query(q);
        assert_eq!(parsed.protocol, Some(crate::types::Protocol::Dash));
        assert_eq!(parsed.pathway, Some("beta".to_string()));
        assert_eq!(parsed.throughput, Some(4_880_000));
    }

    #[test]
    fn parse_dash_query_unquoted_pathway() {
        let q = "_DASH_pathway=alpha&_DASH_throughput=5140000";
        let parsed = parse_query(q);
        assert_eq!(parsed.protocol, Some(crate::types::Protocol::Dash));
        assert_eq!(parsed.pathway, Some("alpha".to_string()));
        assert_eq!(parsed.throughput, Some(5_140_000));
    }

    #[test]
    fn parse_dash_pre_start_no_params() {
        // First request with queryBeforeStart=true: no _DASH_ params
        let q = "token=234523452";
        let parsed = parse_query(q);
        assert_eq!(parsed.protocol, None);
        assert_eq!(parsed.pathway, None);
        assert_eq!(parsed.throughput, None);
        assert_eq!(parsed.passthrough_params.len(), 1);
        assert_eq!(parsed.passthrough_params[0].0, "token");
    }

    // ─── parse_query: token passthrough (Fandango/Akamai style) ─────────

    #[test]
    fn parse_akamai_token_passthrough() {
        let q = "start=1772770805&end=1772857805&userId=93334984&hashParam=a7614ed1\
                 &_HLS_pathway=CDN-A&_HLS_throughput=5000000&_ss=encoded_state";
        let parsed = parse_query(q);
        assert_eq!(parsed.protocol, Some(crate::types::Protocol::Hls));
        assert_eq!(parsed.pathway, Some("CDN-A".to_string()));
        assert_eq!(parsed.throughput, Some(5_000_000));
        assert!(parsed.session_state_raw.is_some());
        // All 4 token params should be passthrough
        assert_eq!(parsed.passthrough_params.len(), 4);
        let keys: Vec<&str> = parsed.passthrough_params.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"start"));
        assert!(keys.contains(&"end"));
        assert!(keys.contains(&"userId"));
        assert!(keys.contains(&"hashParam"));
    }

    #[test]
    fn parse_multiple_custom_params() {
        let q = "session=abc&region=us-east&_HLS_pathway=X&custom=value";
        let parsed = parse_query(q);
        assert_eq!(parsed.passthrough_params.len(), 3);
    }

    // ─── parse_query: edge cases ─────────────────────────────────────────

    #[test]
    fn parse_empty_query() {
        let parsed = parse_query("");
        assert_eq!(parsed.protocol, None);
        assert_eq!(parsed.pathway, None);
        assert_eq!(parsed.throughput, None);
        assert!(parsed.session_state_raw.is_none());
        assert!(parsed.passthrough_params.is_empty());
    }

    #[test]
    fn parse_query_with_empty_segments() {
        let q = "&&_HLS_pathway=A&&";
        let parsed = parse_query(q);
        assert_eq!(parsed.pathway, Some("A".to_string()));
    }

    #[test]
    fn parse_key_without_value() {
        let q = "flag&_HLS_pathway=X";
        let parsed = parse_query(q);
        assert_eq!(parsed.passthrough_params.len(), 1);
        assert_eq!(parsed.passthrough_params[0], ("flag".to_string(), "".to_string()));
    }

    #[test]
    fn parse_invalid_throughput_ignored() {
        let q = "_HLS_throughput=not_a_number";
        let parsed = parse_query(q);
        assert_eq!(parsed.protocol, Some(crate::types::Protocol::Hls));
        assert_eq!(parsed.throughput, None);
    }

    #[test]
    fn parse_zero_throughput() {
        let q = "_DASH_throughput=0";
        let parsed = parse_query(q);
        assert_eq!(parsed.throughput, Some(0));
    }

    #[test]
    fn parse_very_large_throughput() {
        let q = "_HLS_throughput=18446744073709551615"; // u64::MAX
        let parsed = parse_query(q);
        assert_eq!(parsed.throughput, Some(u64::MAX));
    }

    // ─── url_decode ─────────────────────────────────────────────────────

    #[test]
    fn url_decode_percent_encoding() {
        assert_eq!(url_decode("hello%20world"), "hello world");
    }

    #[test]
    fn url_decode_plus_as_space() {
        assert_eq!(url_decode("hello+world"), "hello world");
    }

    #[test]
    fn url_decode_dash_quoted_pathway() {
        assert_eq!(url_decode("%22alpha%22"), "alpha");
    }

    #[test]
    fn url_decode_no_encoding() {
        assert_eq!(url_decode("CDN-A"), "CDN-A");
    }

    #[test]
    fn url_decode_empty() {
        assert_eq!(url_decode(""), "");
    }

    #[test]
    fn url_decode_mixed_case_hex() {
        assert_eq!(url_decode("%2f"), "/");
        assert_eq!(url_decode("%2F"), "/");
    }

    #[test]
    fn url_decode_truncated_percent() {
        // Truncated % at end — should not panic
        let result = url_decode("abc%2");
        assert!(!result.is_empty());
    }

    // ─── build_reload_uri ────────────────────────────────────────────────

    #[test]
    fn build_reload_uri_with_passthrough() {
        let state = SessionState {
            priorities: vec!["alpha".into()],
            ..Default::default()
        };
        let uri = build_reload_uri(
            "/steer",
            &state,
            &[("session".into(), "abc".into()), ("token".into(), "xyz".into())],
        )
        .unwrap();
        assert!(uri.starts_with("/steer?session=abc&token=xyz&_ss="));
    }

    #[test]
    fn build_reload_uri_no_passthrough() {
        let state = SessionState::default();
        let uri = build_reload_uri("/steer", &state, &[]).unwrap();
        assert!(uri.starts_with("/steer?_ss="));
        assert!(!uri.contains("&&"));
    }

    #[test]
    fn build_reload_uri_state_is_decodable() {
        let state = SessionState {
            priorities: vec!["a".into(), "b".into()],
            min_bitrate: 1_000_000,
            ..Default::default()
        };
        let uri = build_reload_uri("/steer", &state, &[]).unwrap();
        let ss_part = uri.split("_ss=").nth(1).unwrap();
        let decoded = decode_state(ss_part).unwrap();
        assert_eq!(decoded.priorities, vec!["a", "b"]);
        assert_eq!(decoded.min_bitrate, 1_000_000);
    }

    #[test]
    fn build_reload_uri_with_absolute_base() {
        let state = SessionState::default();
        let uri = build_reload_uri(
            "https://steer.example.com/v1/steer",
            &state,
            &[("token".into(), "12345".into())],
        )
        .unwrap();
        assert!(uri.starts_with("https://steer.example.com/v1/steer?token=12345&_ss="));
    }

    #[test]
    fn build_reload_uri_preserves_akamai_tokens() {
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
        let uri = build_reload_uri("/steer", &state, &tokens).unwrap();
        assert!(uri.contains("start=1772770805"));
        assert!(uri.contains("end=1772857805"));
        assert!(uri.contains("userId=93334984"));
        assert!(uri.contains("hashParam=a7614ed13747de0802fdd8ff5cd440b4"));
        assert!(uri.contains("_ss="));
    }
}
