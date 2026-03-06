/**
 * Local development HTTP server for apex-edge-steering.
 *
 * Loads the WASM module from pkg/ and serves the three endpoints:
 *   GET  /steer/**  — steering requests (HLS + DASH)
 *   POST /control   — master control plane
 *   GET  /health    — health check
 *
 * Usage:
 *   node scripts/server.mjs [--port 3000]
 */

import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const __dirname = dirname(fileURLToPath(import.meta.url));
const PKG_DIR = join(__dirname, '..', 'pkg');

// ─── WASM Loading ──────────────────────────────────────────────────────────────

async function loadWasm() {
  // Read the glue JS as text so we can extract the import object shape,
  // then manually instantiate the WASM binary.
  const wasmBytes = await readFile(join(PKG_DIR, 'apex_edge_steering_bg.wasm'));

  // We need to provide the __wbg_* imports that the WASM module expects.
  // Import the bg.js glue to get the helper functions, then wire them up.
  const glue = await import(join(PKG_DIR, 'apex_edge_steering_bg.js'));

  const importObject = {
    './apex_edge_steering_bg.js': {
      __wbg_Error_83742b46f01ce22d: glue.__wbg_Error_83742b46f01ce22d,
    },
  };

  const { instance } = await WebAssembly.instantiate(wasmBytes, importObject);
  glue.__wbg_set_wasm(instance.exports);

  return {
    handle_steering_request: glue.handle_steering_request,
    parse_request: glue.parse_request,
    apply_control_command: glue.apply_control_command,
    encode_initial_state: glue.encode_initial_state,
    reset_initial_state: glue.reset_initial_state,
  };
}

// ─── Server State ──────────────────────────────────────────────────────────────

let overridesJson = '';
let configJson = '';
const BASE_PATH = '/steer';

// ─── Request Handling ──────────────────────────────────────────────────────────

function detectProtocol(path, query) {
  if (path.includes('/hls')) return 'hls';
  if (path.includes('/dash')) return 'dash';
  if (query.includes('_HLS_')) return 'hls';
  if (query.includes('_DASH_')) return 'dash';
  return 'hls';
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on('data', (c) => chunks.push(c));
    req.on('end', () => resolve(Buffer.concat(chunks).toString()));
    req.on('error', reject);
  });
}

function respond(res, status, body, extraHeaders = {}) {
  const headers = { 'Content-Type': 'application/json', ...extraHeaders };
  res.writeHead(status, headers);
  res.end(typeof body === 'string' ? body : JSON.stringify(body));
}

async function createHandler(wasm) {
  // Load dev UI HTML once at startup.
  let uiHtml = '';
  try {
    uiHtml = await readFile(join(__dirname, 'ui.html'), 'utf8');
  } catch { /* no UI file — skip */ }

  return async (req, res) => {
    const url = new URL(req.url, `http://${req.headers.host}`);
    const path = url.pathname;
    const query = url.search.slice(1); // remove leading '?'

    // CORS preflight
    if (req.method === 'OPTIONS') {
      res.writeHead(204, {
        'Access-Control-Allow-Origin': '*',
        'Access-Control-Allow-Methods': 'GET, POST, OPTIONS',
        'Access-Control-Allow-Headers': 'Content-Type',
      });
      return res.end();
    }

    // Dev UI
    if (path === '/' && uiHtml) {
      res.writeHead(200, { 'Content-Type': 'text/html' });
      return res.end(uiHtml);
    }

    // Health check
    if (path === '/health') {
      return respond(res, 200, { status: 'ok', engine: 'apex-edge-steering', overrides: overridesJson ? JSON.parse(overridesJson) : null });
    }

    // Control plane: POST /control
    if (path === '/control' && req.method === 'POST') {
      try {
        const body = await readBody(req);
        overridesJson = wasm.apply_control_command(overridesJson, body);
        return respond(res, 200, overridesJson);
      } catch (err) {
        return respond(res, 400, { error: err.message || 'bad command' });
      }
    }

    // Config update: POST /config (dev convenience)
    if (path === '/config' && req.method === 'POST') {
      try {
        const body = await readBody(req);
        JSON.parse(body); // validate
        configJson = body;
        return respond(res, 200, { status: 'config updated', config: JSON.parse(body) });
      } catch (err) {
        return respond(res, 400, { error: err.message || 'bad config' });
      }
    }

    // Config read: GET /config
    if (path === '/config' && req.method === 'GET') {
      return respond(res, 200, configJson || '{}');
    }

    // Encode state: POST /encode-state (dev convenience for manifest updater)
    if (path === '/encode-state' && req.method === 'POST') {
      try {
        const body = await readBody(req);
        const encoded = wasm.encode_initial_state(body);
        return respond(res, 200, { encoded, server_uri: `${BASE_PATH}?_ss=${encoded}` });
      } catch (err) {
        return respond(res, 400, { error: err.message || 'bad state' });
      }
    }

    // Reset: POST /reset (dev convenience)
    if (path === '/reset' && req.method === 'POST') {
      overridesJson = '';
      configJson = '';
      wasm.reset_initial_state();
      return respond(res, 200, { status: 'reset', overrides: null, config: null, initial_state: null });
    }

    // Steering: GET /steer/**
    if (path.startsWith('/steer')) {
      try {
        const protocol = detectProtocol(path, query);
        const requestJson = wasm.parse_request(query, protocol);
        const responseJson = wasm.handle_steering_request(
          requestJson, overridesJson, configJson, BASE_PATH
        );
        return respond(res, 200, responseJson, {
          'Cache-Control': 'no-store, no-cache',
          'Access-Control-Allow-Origin': '*',
        });
      } catch (err) {
        return respond(res, 500, { error: err.message || 'internal error' });
      }
    }

    // 404
    respond(res, 404, { error: 'not found', endpoints: ['GET /steer', 'POST /control', 'GET /health', 'POST /config', 'POST /encode-state', 'POST /reset'] });
  };
}

// ─── Main ──────────────────────────────────────────────────────────────────────

const PORT = parseInt(process.argv.find((_, i, a) => a[i - 1] === '--port') || '3001', 10);

console.log('Loading WASM module...');
const wasm = await loadWasm();
console.log('WASM loaded successfully.');

const handler = await createHandler(wasm);
const server = createServer((req, res) => handler(req, res).catch((err) => {
  console.error('Unhandled:', err);
  respond(res, 500, { error: 'internal server error' });
}));

server.listen(PORT, () => {
  console.log(`\napex-edge-steering dev server listening on http://localhost:${PORT}`);
  console.log(`\nEndpoints:`);
  console.log(`  GET  /                       Dev UI`);
  console.log(`  GET  /steer[/hls|/dash]?...  Steering requests`);
  console.log(`  POST /control                Master control commands`);
  console.log(`  GET  /health                 Health check`);
  console.log(`  POST /config                 Update policy config`);
  console.log(`  POST /encode-state           Encode initial session state`);
  console.log(`  POST /reset                  Reset overrides and config`);
  console.log(`\n  Dev UI: http://localhost:${PORT}/\n`);
});
