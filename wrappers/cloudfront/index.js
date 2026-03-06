/**
 * CloudFront Lambda@Edge wrapper for apex-edge-steering.
 *
 * Deployed as a viewer-request Lambda@Edge function.
 * Routes steering requests to the WASM core, returns steering manifest responses.
 */

const { handle_steering_request, parse_request, apply_control_command } = require('../../pkg/apex_edge_steering');

// In-memory override state.
let overridesJson = '';
let configJson = '';

const BASE_PATH = '/steer';

exports.handler = async (event) => {
  const request = event.Records[0].cf.request;
  const uri = request.uri;
  const querystring = request.querystring || '';

  // Health check
  if (uri === '/health') {
    return {
      status: '200',
      statusDescription: 'OK',
      headers: {
        'content-type': [{ value: 'application/json' }],
        'cache-control': [{ value: 'no-store' }],
      },
      body: JSON.stringify({ status: 'ok', engine: 'apex-edge-steering' }),
    };
  }

  // Control plane: POST /control
  if (uri === '/control' && request.method === 'POST') {
    try {
      const body = request.body?.data
        ? Buffer.from(request.body.data, 'base64').toString('utf-8')
        : '';
      overridesJson = apply_control_command(overridesJson, body);
      return {
        status: '200',
        statusDescription: 'OK',
        headers: {
          'content-type': [{ value: 'application/json' }],
        },
        body: overridesJson,
      };
    } catch (err) {
      return {
        status: '400',
        statusDescription: 'Bad Request',
        headers: { 'content-type': [{ value: 'application/json' }] },
        body: JSON.stringify({ error: err.message }),
      };
    }
  }

  // Steering requests: /steer/**
  if (!uri.startsWith('/steer')) {
    return request; // Pass through
  }

  try {
    const protocol = detectProtocol(uri, querystring);
    const requestJson = parse_request(querystring, protocol);
    const responseJson = handle_steering_request(
      requestJson,
      overridesJson,
      configJson,
      BASE_PATH
    );

    return {
      status: '200',
      statusDescription: 'OK',
      headers: {
        'content-type': [{ value: 'application/json' }],
        'cache-control': [{ value: 'no-store, no-cache' }],
        'access-control-allow-origin': [{ value: '*' }],
      },
      body: responseJson,
    };
  } catch (err) {
    return {
      status: '500',
      statusDescription: 'Internal Server Error',
      headers: { 'content-type': [{ value: 'application/json' }] },
      body: JSON.stringify({ error: err.message || 'internal error' }),
    };
  }
};

function detectProtocol(path, query) {
  if (path.includes('/hls')) return 'hls';
  if (path.includes('/dash')) return 'dash';
  if (query.includes('_HLS_')) return 'hls';
  if (query.includes('_DASH_')) return 'dash';
  return 'hls';
}
