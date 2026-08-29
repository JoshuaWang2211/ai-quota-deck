const NATIVE_HOST = 'me.joshuawang.ai_quota_deck';
const QUOTA_MESSAGE = 'ai-quota-deck:quota';
const REFRESH_MESSAGE = 'ai-quota-deck:refresh';
const REFRESH_ALARM = 'refresh-open-provider-tabs';
const REFRESH_INTERVAL_MINUTES = 3;

let deckPort = null;

function expectedProvider(url) {
  if (url?.startsWith('https://gemini.google.com/')) return 'gemini';
  if (url?.startsWith('https://grok.com/')) return 'grok';
  return null;
}

function validQuotaMessage(message, sender) {
  return message?.type === QUOTA_MESSAGE
    && message.version === 1
    && message.provider === expectedProvider(sender.url)
    && sender.frameId === 0
    && Number.isInteger(message.observed_at)
    && message.observed_at > 0
    && message.payload
    && typeof message.payload === 'object';
}

function connectDeck() {
  if (deckPort) return deckPort;

  try {
    deckPort = chrome.runtime.connectNative(NATIVE_HOST);
    deckPort.onMessage.addListener(() => {});
    deckPort.onDisconnect.addListener(() => {
      void chrome.runtime.lastError;
      deckPort = null;
    });
    return deckPort;
  } catch (error) {
    deckPort = null;
    return null;
  }
}

async function forwardQuota(message) {
  const allowed = await chrome.permissions.contains({ permissions: ['nativeMessaging'] });
  if (!allowed) return;

  const port = connectDeck();
  if (!port) return;
  const { type, ...push } = message;
  try {
    port.postMessage(push);
  } catch (error) {
    deckPort = null;
  }
}

function refreshProviderTab(tab) {
  if (tab.id == null) return;

  // These tabs are the bridge's data source. Letting Memory Saver discard them
  // silently stops monitoring, so opt only these two origins out. A tab that
  // was already discarded has lost its renderer and must be reloaded. A frozen
  // renderer still owns preserved page state (including unsent drafts), so never
  // reload it behind the user's back; visibilitychange refreshes it on return.
  chrome.tabs.update(tab.id, { autoDiscardable: false }, () => {
    void chrome.runtime.lastError;
    if (tab.discarded) {
      chrome.tabs.reload(tab.id, () => void chrome.runtime.lastError);
      return;
    }
    if (tab.frozen) return;
    chrome.tabs.sendMessage(tab.id, { type: REFRESH_MESSAGE }, () => {
      void chrome.runtime.lastError;
    });
  });
}

function refreshOpenProviderTabs() {
  chrome.tabs.query({
    url: ['https://gemini.google.com/*', 'https://grok.com/*']
  }, (tabs) => {
    void chrome.runtime.lastError;
    for (const tab of tabs || []) refreshProviderTab(tab);
  });
}

async function restoreDeckConnection() {
  const allowed = await chrome.permissions.contains({ permissions: ['nativeMessaging'] });
  if (!allowed) return;
  connectDeck();
  refreshOpenProviderTabs();
}

async function ensureRefreshAlarm() {
  const alarm = await chrome.alarms.get(REFRESH_ALARM);
  if (!alarm) {
    await chrome.alarms.create(REFRESH_ALARM, {
      periodInMinutes: REFRESH_INTERVAL_MINUTES
    });
  }
}

chrome.runtime.onMessage.addListener((message, sender) => {
  if (!validQuotaMessage(message, sender)) return;
  void forwardQuota(message);
});

chrome.action.onClicked.addListener(() => {
  // Chrome requires the optional permission request to remain directly inside
  // a user gesture. A denial leaves the provider pages entirely untouched.
  chrome.permissions.request({ permissions: ['nativeMessaging'] }, (granted) => {
    void chrome.runtime.lastError;
    if (!granted) return;
    connectDeck();
    refreshOpenProviderTabs();
  });
});

chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === REFRESH_ALARM) refreshOpenProviderTabs();
});

// Alarm persistence before Chrome 150 is not guaranteed across every browser
// restart. Registering onStartup gives the worker an event that can recreate
// the schedule even when no provider tab has produced a message yet.
chrome.runtime.onStartup.addListener(() => {
  void restoreDeckConnection();
  void ensureRefreshAlarm();
});

// Windows can lock the interactive session without putting the machine to
// sleep. Chromium may freeze background renderers during that interval, so an
// explicit active transition is the reliable point to revive and refresh them.
chrome.idle.onStateChanged.addListener((state) => {
  if (state === 'active') refreshOpenProviderTabs();
});

// The permission prompt itself must stay inside the toolbar gesture above, but
// an already-granted permission can be reused after a browser or service-worker
// restart. Reconnect and ask existing provider tabs for a fresh snapshot.
void restoreDeckConnection();
void ensureRefreshAlarm();
