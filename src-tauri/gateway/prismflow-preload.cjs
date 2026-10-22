// PrismFlow preload for the gateway child (MIT, PrismFlow's own code).
// The broker holds this process's stdin open; when PrismFlow exits for any
// reason, including a crash, stdin reaches EOF and the gateway exits too.
// That prevents an orphaned gateway from holding upstream keys in its env.
'use strict';
process.stdin.on('end', () => process.exit(0));
process.stdin.on('error', () => process.exit(0));
process.stdin.resume();

// A provider stream that stops without its closing marker was cut off (a
// dropped connection, a crashed server). Left alone, it reads as a finished
// answer and the app shows half a reply as if it were whole. Turn that quiet
// end into a stream error, which the engine reports to the app as a failure.
// Covers OpenAI Chat Completions ([DONE] or a finish_reason) and Responses
// (a response.completed / .incomplete / .failed event) streams.
const ENDED = {
  chat: /data:\s*\[DONE\]|"finish_reason"\s*:\s*"[^"]+"/,
  responses: /"type"\s*:\s*"response\.(completed|incomplete|failed)"|event:\s*response\.(completed|incomplete|failed)|"type"\s*:\s*"error"/,
};

function kindOf(input) {
  let url = '';
  try { url = typeof input === 'string' ? input : (input && input.url) || String(input); } catch { return null; }
  const path = url.split('?')[0];
  if (/\/chat\/completions\/?$/.test(path)) return 'chat';
  if (/\/responses\/?$/.test(path)) return 'responses';
  return null;
}

function guard(kind, body) {
  let seen = false;
  let tail = '';
  const decoder = new TextDecoder();
  return body.pipeThrough(new TransformStream({
    transform(chunk, controller) {
      if (!seen) {
        const text = tail + decoder.decode(chunk, { stream: true });
        seen = ENDED[kind].test(text);
        tail = text.slice(-256);
      }
      controller.enqueue(chunk);
    },
    flush(controller) {
      if (!seen && !ENDED[kind].test(tail + decoder.decode())) {
        controller.error(new Error('The provider stopped in the middle of its answer.'));
      }
    },
  }));
}

// Whether the app asked for a stream; providers don't always label one.
function asksToStream(init, type) {
  if (type.includes('text/event-stream')) return true;
  let body = init && init.body;
  if (body instanceof Uint8Array) body = new TextDecoder().decode(body);
  return typeof body === 'string' && /"stream"\s*:\s*true/.test(body);
}

// Seconds until a rate limit lifts, from whichever header the provider uses:
// Retry-After (seconds or a date), x-ratelimit-reset-* ("37s", "1m30s",
// "250ms"), x-ratelimit-reset (epoch or seconds), anthropic-*-reset (a date).
function retryAfterSeconds(headers) {
  const dur = (v) => {
    if (!v) return null;
    v = String(v).trim();
    if (/^\d+(\.\d+)?$/.test(v)) {
      const n = Number(v);
      return n > 1e9 ? Math.max(0, n - Date.now() / 1000) : n; // epoch or seconds
    }
    const parts = [...v.matchAll(/(\d+(?:\.\d+)?)(ms|h|m|s)/g)];
    if (parts.length) return parts.reduce((t, [, n, u]) => t + Number(n) * ({ ms: 0.001, s: 1, m: 60, h: 3600 })[u], 0);
    const d = Date.parse(v);
    return Number.isNaN(d) ? null : Math.max(0, (d - Date.now()) / 1000);
  };
  const names = ['retry-after', 'x-ratelimit-reset-requests', 'x-ratelimit-reset-tokens', 'x-ratelimit-reset',
    'anthropic-ratelimit-requests-reset', 'anthropic-ratelimit-tokens-reset', 'anthropic-ratelimit-input-tokens-reset'];
  const found = names.map((n) => dur(headers.get(n))).filter((n) => n !== null && Number.isFinite(n));
  return found.length ? Math.ceil(Math.max(...found)) : null;
}

// Limit answers (429, 402) lose their headers on the way through the engine;
// put the wait into the body, which the engine passes on as details.
async function withRetryAfter(res, ResponseClass) {
  if (res.status !== 429 && res.status !== 402) return res;
  const secs = retryAfterSeconds(res.headers);
  if (secs === null) return res;
  let text = '';
  try { text = await res.text(); } catch { return res; }
  let body;
  try { body = JSON.parse(text); } catch { body = { message: text }; }
  if (body && typeof body === 'object' && !Array.isArray(body)) body.prismflow_retry_after = secs;
  const wrapped = new ResponseClass(JSON.stringify(body), { status: res.status, statusText: res.statusText, headers: res.headers });
  Object.defineProperty(wrapped, 'url', { value: res.url });
  return wrapped;
}

function wrapFetch(realFetch, ResponseClass) {
  return async function prismflowFetch(input, init) {
    const res = await withRetryAfter(await realFetch(input, init), ResponseClass);
    const kind = kindOf(input);
    const type = res.headers.get('content-type') || '';
    if (!kind || !res.ok || !res.body || !asksToStream(init, type)) return res;
    const wrapped = new ResponseClass(guard(kind, res.body), { status: res.status, statusText: res.statusText, headers: res.headers });
    Object.defineProperty(wrapped, 'url', { value: res.url });
    Object.defineProperty(wrapped, 'redirected', { value: res.redirected });
    return wrapped;
  };
}

// The engine calls providers through undici's fetch (and the global fetch
// in a few places); wrap both.
if (typeof globalThis.fetch === 'function') {
  globalThis.fetch = wrapFetch(globalThis.fetch, globalThis.Response);
}
const Module = require('node:module');
const realLoad = Module._load;
Module._load = function prismflowLoad(request, parent, isMain) {
  const mod = realLoad.apply(this, arguments);
  if (request === 'undici' && mod && typeof mod.fetch === 'function' && !mod.fetch.prismflowGuarded) {
    const guarded = wrapFetch(mod.fetch, mod.Response || globalThis.Response);
    guarded.prismflowGuarded = true;
    try { mod.fetch = guarded; } catch { /* read-only: leave as is */ }
  }
  return mod;
};
