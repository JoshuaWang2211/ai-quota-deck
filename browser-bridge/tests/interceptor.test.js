const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const posted = [];
const windowStub = {
  WIZ_global_data: { SNlM0e: 'test-token' },
  postMessage(message) {
    posted.push(message);
  },
  addEventListener() {}
};

const context = vm.createContext({
  window: windowStub,
  setTimeout() {},
  setInterval() {}
});

// Gemini Usage Monitor already declares these names in the page's MAIN world.
// The bridge must coexist regardless of which extension Chrome injects first.
const siblingGlobals = `
  const INITIAL_RETRY_DELAYS_MS = [];
  const PERIODIC_REFRESH_MS = 1;
  function postToken() {}
`;
vm.runInContext(siblingGlobals, context);

const interceptor = fs.readFileSync(
  path.join(__dirname, '..', 'src', 'gemini-interceptor.js'),
  'utf8'
);

assert.doesNotThrow(() => vm.runInContext(interceptor, context));
assert.equal(posted.length, 1);
assert.equal(posted[0].type, 'AI_QUOTA_DECK_GEMINI_AT_TOKEN');
assert.equal(posted[0].token, 'test-token');

const reverseContext = vm.createContext({
  window: windowStub,
  setTimeout() {},
  setInterval() {}
});
assert.doesNotThrow(() => vm.runInContext(interceptor, reverseContext));
assert.doesNotThrow(() => vm.runInContext(siblingGlobals, reverseContext));

console.log('Gemini MAIN-world interceptor coexists with sibling extension globals: ok');
