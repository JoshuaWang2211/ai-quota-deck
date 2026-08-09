# AI Quota Deck Browser Bridge

Advanced companion Chromium extension for the browser-only Gemini and Grok quota
sources. Claude and Codex work without it. Gemini requires the bridge; Grok uses
it as the preferred source but can fall back to a signed-in Grok Build.
It has no page UI and does not depend on or modify the existing
`gemini-usage-monitor` and `grok-usage-watch` extensions.
Its Gemini MAIN-world token relay is privately scoped so it can coexist with
Gemini Usage Monitor on the same page.

This directory is what ships. It is bundled into the installer and staged to
`%LOCALAPPDATA%\ai-quota-deck\browser-bridge` on app startup; users load that
copy, not this one. The existing Chrome Web Store extensions remain unchanged.
See the root `README.md` for the user-facing setup steps.

Extension ID: `alckoeangnmpomfnafaajjbpniomhnke`. The manifest contains a public
key so the unpacked ID stays stable across Chromium profiles.

For development, load this directory directly from `chrome://extensions`, then
click the toolbar action once to grant the optional `nativeMessaging`
permission. Keep the relevant provider tab open when a fresh snapshot is needed.

Only quota numbers, reset timestamps, provider name, account slot (`uN` for
Gemini), and observation time cross Native Messaging. Cookies and page tokens
never leave the provider tab.
