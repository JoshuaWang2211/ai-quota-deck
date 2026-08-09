(function runGrokBridge() {
  const QUOTA_MESSAGE = 'ai-quota-deck:quota';
  const REFRESH_MESSAGE = 'ai-quota-deck:refresh';
  const CREDITS_SERVICE = 'grok_api_v2.GrokBuildBilling';
  const CREDITS_METHOD = 'GetGrokCreditsConfig';
  const CACHE_WINDOW_MS = 50 * 1000;
  const DATA_KEY = 'aiQuotaDeckGrokData';
  const FETCH_TIME_KEY = 'aiQuotaDeckGrokFetchTime';

  function pushQuota(data, observedAtMs) {
    chrome.runtime.sendMessage({
      type: QUOTA_MESSAGE,
      version: 1,
      provider: 'grok',
      observed_at: Math.floor(observedAtMs / 1000),
      payload: data
    }, () => void chrome.runtime.lastError);
  }

  async function fetchActiveSubscription() {
    try {
      const response = await fetch('https://grok.com/rest/subscriptions', {
        credentials: 'include'
      });
      if (!response.ok) return null;
      const data = await response.json();
      const subscriptions = Array.isArray(data?.subscriptions) ? data.subscriptions : [];
      return subscriptions.some((entry) => entry?.status === 'SUBSCRIPTION_STATUS_ACTIVE');
    } catch (error) {
      return null;
    }
  }

  async function fetchFreeUsage() {
    const response = await fetch('https://grok.com/rest/rate-limits', {
      method: 'POST',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ requestKind: 'DEFAULT', modelName: 'grok-3' })
    });
    if (!response.ok) return null;
    const data = await response.json();
    const total = Number(data.totalQueries);
    const remaining = Number(data.remainingQueries);
    if (!Number.isFinite(total) || total <= 0 || !Number.isFinite(remaining)) return null;
    const percentLeft = Math.round((remaining / total) * 100);
    return {
      buckets: [{
        key: 'grok-3',
        label: 'Fast',
        remaining,
        total,
        percent: percentLeft,
        used: 100 - percentLeft
      }],
      unauthorized: false,
      paid: null
    };
  }

  async function fetchPaidUsage() {
    const response = await fetch(`https://grok.com/${CREDITS_SERVICE}/${CREDITS_METHOD}`, {
      method: 'POST',
      credentials: 'include',
      headers: {
        'Content-Type': 'application/grpc-web+proto',
        'X-Grpc-Web': '1'
      },
      body: new Uint8Array(5)
    });
    if (!response.ok) return null;
    const frame = AiQuotaDeckGrokParser.firstGrpcDataFrame(await response.arrayBuffer());
    const paid = frame && AiQuotaDeckGrokParser.readPaidUsage(frame);
    return paid ? { buckets: [], unauthorized: false, paid } : null;
  }

  async function refresh(force = false) {
    if (!force) {
      try {
        const stored = await chrome.storage.local.get([DATA_KEY, FETCH_TIME_KEY]);
        const fetchedAt = stored[FETCH_TIME_KEY];
        if (stored[DATA_KEY] && Number.isFinite(fetchedAt)
            && Date.now() - fetchedAt < CACHE_WINDOW_MS) {
          pushQuota(stored[DATA_KEY], fetchedAt);
          return;
        }
      } catch (error) {
        // A storage failure should not block a live provider fetch.
      }
    }

    const paidAccount = await fetchActiveSubscription();
    if (paidAccount == null) return;
    const data = paidAccount ? await fetchPaidUsage() : await fetchFreeUsage();
    if (!data) return;

    const fetchedAt = Date.now();
    await chrome.storage.local.set({ [DATA_KEY]: data, [FETCH_TIME_KEY]: fetchedAt });
    pushQuota(data, fetchedAt);
  }

  chrome.runtime.onMessage.addListener((message) => {
    if (message?.type === REFRESH_MESSAGE) void refresh(true);
  });

  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) void refresh();
  });

  void refresh();
})();
