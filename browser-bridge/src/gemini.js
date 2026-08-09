(function runGeminiBridge() {
  const QUOTA_MESSAGE = 'ai-quota-deck:quota';
  const REFRESH_MESSAGE = 'ai-quota-deck:refresh';
  const TOKEN_MESSAGE = 'AI_QUOTA_DECK_GEMINI_AT_TOKEN';
  const TOKEN_REFRESH_MESSAGE = 'AI_QUOTA_DECK_GEMINI_REFRESH_TOKEN';
  const CACHE_WINDOW_MS = 25 * 1000;
  const TOKEN_REFRESH_COOLDOWN_MS = 60 * 1000;

  const accountId = (location.pathname.match(/^\/u\/(\d+)(\/|$)/) || [])[1] || '0';
  const accountPrefix = accountId === '0' ? '' : `/u/${accountId}`;
  const accountSuffix = accountId === '0' ? '' : `_u${accountId}`;
  const apiUrl = `https://gemini.google.com${accountPrefix}/_/BardChatUi/data/batchexecute?rpcids=jSf9Qc&source-path=/usage`;
  const dataKey = `aiQuotaDeckGeminiData${accountSuffix}`;
  const fetchTimeKey = `aiQuotaDeckGeminiFetchTime${accountSuffix}`;

  let atToken = '';
  let lastTokenRefreshRequest = 0;
  let pollStarted = false;

  function pushQuota(data, observedAtMs) {
    chrome.runtime.sendMessage({
      type: QUOTA_MESSAGE,
      version: 1,
      provider: 'gemini',
      observed_at: Math.floor(observedAtMs / 1000),
      payload: { account_id: accountId, ...data }
    }, () => void chrome.runtime.lastError);
  }

  function requestFreshToken() {
    if (Date.now() - lastTokenRefreshRequest <= TOKEN_REFRESH_COOLDOWN_MS) return;
    lastTokenRefreshRequest = Date.now();
    window.postMessage({ type: TOKEN_REFRESH_MESSAGE }, '*');
  }

  async function fetchAndPush(force = false) {
    if (!atToken) {
      requestFreshToken();
      return;
    }

    if (!force) {
      try {
        const stored = await chrome.storage.local.get([dataKey, fetchTimeKey]);
        const fetchedAt = stored[fetchTimeKey];
        if (stored[dataKey] && Number.isFinite(fetchedAt)
            && Date.now() - fetchedAt < CACHE_WINDOW_MS) {
          pushQuota(stored[dataKey], fetchedAt);
          return;
        }
      } catch (error) {
        // A storage failure should not block a live provider fetch.
      }
    }

    try {
      const response = await fetch(apiUrl, {
        method: 'POST',
        headers: { 'Content-Type': 'application/x-www-form-urlencoded;charset=UTF-8' },
        body: new URLSearchParams({
          'f.req': '[[["jSf9Qc","[]",null,"generic"]]]',
          at: atToken
        }).toString()
      });
      if (!response.ok) throw new Error(`Gemini quota returned ${response.status}`);
      const data = AiQuotaDeckGeminiParser.parseLimits(await response.text());
      if (!data) throw new Error('Gemini quota response did not contain both windows');

      const fetchedAt = Date.now();
      await chrome.storage.local.set({ [dataKey]: data, [fetchTimeKey]: fetchedAt });
      pushQuota(data, fetchedAt);
    } catch (error) {
      requestFreshToken();
    }
  }

  window.addEventListener('message', (event) => {
    if (event.source !== window || event.data?.type !== TOKEN_MESSAGE || !event.data.token) return;
    atToken = event.data.token;
    if (!pollStarted) {
      pollStarted = true;
      void fetchAndPush();
    }
  });

  chrome.runtime.onMessage.addListener((message) => {
    if (message?.type === REFRESH_MESSAGE) void fetchAndPush(true);
  });

  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) void fetchAndPush();
  });

  window.postMessage({ type: TOKEN_REFRESH_MESSAGE }, '*');
})();
