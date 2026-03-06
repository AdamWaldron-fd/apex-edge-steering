/**
 * Fastly Compute@Edge wrapper for apex-edge-steering.
 *
 * Fastly Compute runs WASM natively. This wrapper uses the Fastly JS SDK
 * which internally compiles to WASM.
 */

import { handle_steering_request, parse_request, apply_control_command } from '../../pkg/apex_edge_steering';

// In-memory override state.
let overridesJson = '';
let configJson = '';

const BASE_PATH = '/steer';

addEventListener('fetch', (event) => {
  event.respondWith(handleRequest(event.request));
});

async function handleRequest(request) {
  const url = new URL(request.url);
  const path = url.pathname;
  const query = url.search.slice(1);

  // Health
  if (path === '/health') {
    return new Response(
      JSON.stringify({ status: 'ok', engine: 'apex-edge-steering' }),
      { headers: { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' } }
    );
  }

  // Control plane
  if (path === '/control' && request.method === 'POST') {
    try {
      const body = await request.text();
      overridesJson = apply_control_command(overridesJson, body);
      return new Response(overridesJson, {
        headers: { 'Content-Type': 'application/json' },
      });
    } catch (err) {
      return new Response(
        JSON.stringify({ error: err.message }),
        { status: 400, headers: { 'Content-Type': 'application/json' } }
      );
    }
  }

  // Steering
  if (path.startsWith('/steer')) {
    try {
      const protocol = detectProtocol(path, query);
      const requestJson = parse_request(query, protocol);
      const responseJson = handle_steering_request(
        requestJson,
        overridesJson,
        configJson,
        BASE_PATH
      );
      return new Response(responseJson, {
        headers: {
          'Content-Type': 'application/json',
          'Cache-Control': 'no-store, no-cache',
          'Access-Control-Allow-Origin': '*',
        },
      });
    } catch (err) {
      return new Response(
        JSON.stringify({ error: err.message || 'internal error' }),
        { status: 500, headers: { 'Content-Type': 'application/json' } }
      );
    }
  }

  return new Response('Not Found', { status: 404 });
}

function detectProtocol(path, query) {
  if (path.includes('/hls')) return 'hls';
  if (path.includes('/dash')) return 'dash';
  if (query.includes('_HLS_')) return 'hls';
  if (query.includes('_DASH_')) return 'dash';
  return 'hls';
}
