const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const flush = () => new Promise((resolve) => setImmediate(resolve));

test('Gemini retries immediately when the interceptor supplies a new token', async () => {
  const listeners = {};
  const posted = [];
  const pushed = [];
  let fetchCalls = 0;
  const windowStub = {
    postMessage(message) { posted.push(message); },
    addEventListener(name, listener) { listeners[name] = listener; }
  };
  const context = vm.createContext({
    window: windowStub,
    location: { pathname: '/' },
    document: {
      hidden: true,
      addEventListener(name, listener) { listeners[`document:${name}`] = listener; }
    },
    chrome: {
      runtime: {
        lastError: null,
        sendMessage(message, callback) { pushed.push(message); callback(); },
        onMessage: { addListener(listener) { listeners.runtime = listener; } }
      },
      storage: {
        local: {
          async get() { return {}; },
          async set() { throw new Error('simulated storage failure'); }
        }
      }
    },
    AiQuotaDeckGeminiParser: {
      parseLimits() { return { ratio5h: 0.1, ratio7d: 0.2 }; }
    },
    async fetch() {
      fetchCalls += 1;
      if (fetchCalls === 1) return { ok: false, status: 401 };
      return { ok: true, async text() { return 'quota'; } };
    },
    URLSearchParams,
    Date
  });
  const script = fs.readFileSync(path.join(__dirname, '..', 'src', 'gemini.js'), 'utf8');
  vm.runInContext(script, context);

  listeners.message({
    source: windowStub,
    data: { type: 'AI_QUOTA_DECK_GEMINI_AT_TOKEN', token: 'stale-token' }
  });
  await flush();
  await flush();
  assert.equal(fetchCalls, 1);
  assert.equal(posted.at(-1).type, 'AI_QUOTA_DECK_GEMINI_REFRESH_TOKEN');

  listeners.message({
    source: windowStub,
    data: { type: 'AI_QUOTA_DECK_GEMINI_AT_TOKEN', token: 'fresh-token' }
  });
  await flush();
  await flush();

  assert.equal(fetchCalls, 2);
  assert.equal(pushed.length, 1, 'a browser-cache failure must not discard live quota');
  assert.equal(pushed[0].provider, 'gemini');
});

test('Grok pushes live quota even when browser storage is unavailable', async () => {
  const listeners = {};
  const pushed = [];
  let fetchCalls = 0;
  const context = vm.createContext({
    document: {
      hidden: true,
      addEventListener(name, listener) { listeners[name] = listener; }
    },
    chrome: {
      runtime: {
        lastError: null,
        sendMessage(message, callback) { pushed.push(message); callback(); },
        onMessage: { addListener(listener) { listeners.runtime = listener; } }
      },
      storage: {
        local: {
          async get() { return {}; },
          async set() { throw new Error('simulated storage failure'); }
        }
      }
    },
    async fetch() {
      fetchCalls += 1;
      if (fetchCalls === 1) {
        return { ok: true, async json() { return { subscriptions: [] }; } };
      }
      return {
        ok: true,
        async json() { return { totalQueries: 2, remainingQueries: 1 }; }
      };
    },
    AiQuotaDeckGrokParser: {},
    Uint8Array,
    Date
  });
  const script = fs.readFileSync(path.join(__dirname, '..', 'src', 'grok.js'), 'utf8');
  vm.runInContext(script, context);
  await flush();
  await flush();
  await flush();

  assert.equal(fetchCalls, 2);
  assert.equal(pushed.length, 1);
  assert.equal(pushed[0].provider, 'grok');
  assert.equal(pushed[0].payload.buckets[0].remaining, 1);
});
