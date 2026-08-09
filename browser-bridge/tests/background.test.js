const assert = require('node:assert/strict');

let actionListener;
let runtimeListener;
let runtimeStartupListener;
let alarmListener;
let idleListener;
const nativeMessages = [];
const refreshedTabs = [];
const updatedTabs = [];
const reloadedTabs = [];
let nativeConnections = 0;
let createdAlarm = null;

global.chrome = {
  alarms: {
    async get(name) {
      assert.equal(name, 'refresh-open-provider-tabs');
      return null;
    },
    async create(name, options) {
      createdAlarm = [name, options];
    },
    onAlarm: { addListener(listener) { alarmListener = listener; } }
  },
  action: {
    onClicked: { addListener(listener) { actionListener = listener; } }
  },
  idle: {
    onStateChanged: { addListener(listener) { idleListener = listener; } }
  },
  permissions: {
    async contains() { return true; },
    request(permission, callback) {
      assert.deepEqual(permission, { permissions: ['nativeMessaging'] });
      callback(true);
    }
  },
  runtime: {
    lastError: null,
    onMessage: { addListener(listener) { runtimeListener = listener; } },
    onStartup: { addListener(listener) { runtimeStartupListener = listener; } },
    connectNative(host) {
      nativeConnections += 1;
      assert.equal(host, 'me.joshuawang.ai_quota_deck');
      return {
        onMessage: { addListener() {} },
        onDisconnect: { addListener() {} },
        postMessage(message) { nativeMessages.push(message); }
      };
    }
  },
  tabs: {
    query(query, callback) {
      assert.deepEqual(query.url, ['https://gemini.google.com/*', 'https://grok.com/*']);
      callback([{ id: 7 }, { id: 9, discarded: true }, { id: 11, frozen: true }]);
    },
    update(tabId, properties, callback) {
      updatedTabs.push([tabId, properties]);
      callback();
    },
    reload(tabId, callback) {
      reloadedTabs.push(tabId);
      callback();
    },
    sendMessage(tabId, message, callback) {
      refreshedTabs.push([tabId, message]);
      callback();
    }
  }
};

require('../src/background.js');

actionListener();
assert.equal(nativeConnections, 1);
assert.deepEqual(refreshedTabs, [
  [7, { type: 'ai-quota-deck:refresh' }]
]);
alarmListener({ name: 'unrelated' });
alarmListener({ name: 'refresh-open-provider-tabs' });
idleListener('locked');
idleListener('active');
runtimeStartupListener();

const gemini = {
  type: 'ai-quota-deck:quota',
  version: 1,
  provider: 'gemini',
  observed_at: 1800000000,
  payload: { account_id: '0', ratio5h: 0.1, ratio7d: 0.2 }
};
runtimeListener(gemini, {
  frameId: 0,
  url: 'https://gemini.google.com/'
});
runtimeListener({ ...gemini, provider: 'grok' }, {
  frameId: 0,
  url: 'https://gemini.google.com/'
});
runtimeListener(gemini, {
  frameId: 1,
  url: 'https://gemini.google.com/'
});

setImmediate(() => {
  assert.deepEqual(createdAlarm, [
    'refresh-open-provider-tabs',
    { periodInMinutes: 3 }
  ]);
  assert.equal(refreshedTabs.length, 5,
    'worker start, browser startup, toolbar click, alarm, and active should refresh loaded tabs');
  assert.deepEqual(reloadedTabs, [9, 11, 9, 11, 9, 11, 9, 11, 9, 11],
    'discarded and frozen tabs should be revived on every recovery trigger');
  assert.deepEqual(updatedTabs.slice(0, 3), [
    [7, { autoDiscardable: false }],
    [9, { autoDiscardable: false }],
    [11, { autoDiscardable: false }]
  ]);
  assert.deepEqual(nativeMessages, [{
    version: 1,
    provider: 'gemini',
    observed_at: 1800000000,
    payload: gemini.payload
  }]);
  assert.equal(nativeConnections, 1, 'forwarding should reuse the open native port');
  console.log('background permission, refresh, origin/frame routing: ok');
});
