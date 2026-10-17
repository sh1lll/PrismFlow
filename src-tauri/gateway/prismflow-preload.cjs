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

function wrapFetch(realFetch, ResponseClass) {
  return async function prismflowFetch(input, init) {
    const res = await realFetch(input, init);
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
