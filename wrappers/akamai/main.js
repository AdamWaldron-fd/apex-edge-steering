/**
 * Akamai EdgeWorkers wrapper for apex-edge-steering.
 *
 * EdgeWorkers entry point: handles incoming steering requests in the
 * onClientRequest event handler. Uses the WASM core for all protocol
 * logic, keeping the JS layer as thin as possible.
 *
 * Deployment: bundle this file with the WASM pkg into an EdgeWorker bundle.
 */

import { handle_steering_request, parse_request, apply_control_command } from '../../pkg/apex_edge_steering';

// In-memory override state. Updated via sub-requests to the control endpoint
// or via EdgeKV reads. Persists for the lifetime of the EdgeWorker instance.
let overridesJson = '';

// Configuration (can be loaded from EdgeKV or property variables).
let configJson = '';

// Base path for constructing RELOAD-URIs.
const BASE_PATH = '/steer';

/**
 * Detect protocol from the request path or a query hint.
 * Convention: /steer/hls or /steer/dash, or ?protocol=hls|dash
 */
function detectProtocol(path, query) {
  if (path.includes('/hls')) return 'hls';
  if (path.includes('/dash')) return 'dash';
  // Fallback: check for _HLS_ or _DASH_ params in query
  if (query.includes('_HLS_')) return 'hls';
  if (query.includes('_DASH_')) return 'dash';
  return 'hls'; // default
}

/**
 * EdgeWorker onClientRequest handler.
 * Intercepts steering requests and returns the steering manifest response.
 */
export async function onClientRequest(request) {
  const path = request.path;
  const query = request.query || '';

  // Control plane endpoint: POST /control
  if (path.startsWith('/control') && request.method === 'POST') {
    return handleControlRequest(request);
  }

  // Health check
  if (path === '/health') {
    return request.respondWith(200, {
      'Content-Type': 'application/json',
      'Cache-Control': 'no-store',
    }, JSON.stringify({ status: 'ok', engine: 'apex-edge-steering' }));
  }

  // Steering request: GET /steer/**
  if (!path.startsWith('/steer')) {
    return; // Pass through non-steering requests
  }

  try {
    const protocol = detectProtocol(path, query);

    // Parse the raw query string into a SteeringRequest via WASM.
    const requestJson = parse_request(query, protocol);

    // Process the steering request through the WASM core.
    const responseJson = handle_steering_request(
      requestJson,
      overridesJson,
      configJson,
      BASE_PATH
    );

    request.respondWith(200, {
      'Content-Type': 'application/json',
      'Cache-Control': 'no-store, no-cache',
      'Access-Control-Allow-Origin': '*',
    }, responseJson);
  } catch (err) {
    request.respondWith(500, {
      'Content-Type': 'application/json',
      'Cache-Control': 'no-store',
    }, JSON.stringify({ error: err.message || 'internal error' }));
  }
}

/**
 * Handle control plane commands from the master steering server.
 * Expects JSON body with a ControlCommand.
 */
async function handleControlRequest(request) {
  try {
    const body = await request.text();
    overridesJson = apply_control_command(overridesJson, body);
    request.respondWith(200, {
      'Content-Type': 'application/json',
    }, overridesJson);
  } catch (err) {
    request.respondWith(400, {
      'Content-Type': 'application/json',
    }, JSON.stringify({ error: err.message || 'bad command' }));
  }
}
