// MAIN world: the WIZ token is page state and is not visible to an isolated
// content script. It never leaves this tab; the content script uses it only in
// a same-origin Gemini request.
//
// Keep every declaration inside a private scope. MAIN-world scripts from every
// installed extension share the page's global lexical environment, so a common
// top-level name can make the next extension's entire interceptor fail with
// "Identifier has already been declared".
(function installGeminiTokenRelay() {
  const TOKEN_MESSAGE = 'AI_QUOTA_DECK_GEMINI_AT_TOKEN';
  const REFRESH_MESSAGE = 'AI_QUOTA_DECK_GEMINI_REFRESH_TOKEN';
  const INITIAL_RETRY_DELAYS_MS = [1000, 3000, 7000, 15000, 30000];
  const PERIODIC_REFRESH_MS = 10 * 60 * 1000;

  function postToken() {
    const token = window.WIZ_global_data?.SNlM0e;
    if (token) window.postMessage({ type: TOKEN_MESSAGE, token }, '*');
  }

  postToken();
  INITIAL_RETRY_DELAYS_MS.forEach((delay) => setTimeout(postToken, delay));
  setInterval(postToken, PERIODIC_REFRESH_MS);

  window.addEventListener('message', (event) => {
    if (event.source === window && event.data?.type === REFRESH_MESSAGE) postToken();
  });
})();
