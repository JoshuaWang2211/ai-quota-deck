# 📜 Changelog – AI Quota Deck

All notable changes to AI Quota Deck are documented here.

---

## v0.2.2 - 2026-08-14

- Added a 10-second timeout to the Codex and Grok network requests. A single stalled provider request could previously freeze the whole refresh cycle indefinitely.
- Hardened the Grok browser parser: a paid-usage response whose fields no longer decode is discarded instead of overwriting a real snapshot with a fabricated 0%. A genuine 0% (fresh account, new period) still lands.
- Made browser-cache writes atomic (write-aside and rename), so concurrent extension pushes and dashboard reads can no longer observe a half-written file — which could also bypass the free-tier/paid-snapshot protection.
- Fixed Grok Build credential selection: a still-usable sign-in is no longer outranked by an expired entry that happens to carry a timestamp.
- Moved Claude's first CLI version scan off the async runtime, and stopped killing a still-running `claude update` mid-replace; a failed automatic token refresh now waits an hour before the updater may run again.
- The dashboard now honors the 429 cooldown reported by the backend directly instead of keeping a duplicate backoff table.

---

## v0.2.1 - 2026-08-11

- Added automatic Claude OAuth recovery: after a rejected access token, the deck runs the official `claude update` command in the background, rereads Claude Code's credential file, and retries once.
- Fixed Widget/Strip positions being overwritten or visually reset by Windows-generated moves, sleep/display wake-up, mixed-DPI monitor checks, or content-driven resizing.
- Reasserted the saved companion position and always-on-top state while monitors settle after wake.
- Removed Strip's accidental double-click mode switch; its dashboard button remains the explicit way to leave Strip view.

---

## v0.2.0 - 2026-08-10

- Replaced Mini mode with three focused views: Dashboard, an always-on-top Widget, and a compact movable Strip.
- Added compact color-coded quota values, separately remembered Widget/Strip positions, Widget movement lock/unlock, and tray controls for both companion views.
- Kept the dashboard as the only provider poller; Widget and Strip receive quota snapshots without adding provider requests.
- Made Claude wake recovery more conservative: checks wait one minute after resume, use a six-minute active polling floor, recover from expired cooldowns through a native background tick, and show cached failure reasons with retry countdowns.

---

## v0.1.2 - 2026-08-10

- Fixed premature and duplicate desktop shortcuts. The installer now creates one **AI Quota Deck** shortcut only when the finish-page option is selected.
- Standardized the installer, install directory, Start menu, and uninstall entry on the **AI Quota Deck** product name.

---

## v0.1.1 - 2026-08-09

- Added installer handling intended to restore a missing **AI Quota Deck** desktop shortcut during reinstalls and updates.

---

## v0.1.0 - 2026-08-09

- Initial Windows release with Claude, Codex, Gemini, and Grok quota cards.
- Added full and Mini modes, light/dark themes, pace indicators, plan badges, reset times, and cached-data status.
- Added Windows tray controls, optional background launch at startup, single-instance handling, and visible launches from shortcuts or the installer.
- Added local Claude Code and Codex Desktop sign-in discovery without token refresh.
- Added Claude idle/lock suspension, persisted rate-limit cooldowns, and cached quota fallback.
- Added browser-first Gemini and Grok collection through the bundled Browser Bridge, including wake/unlock recovery.
- Added Grok Build fallback and protection against anonymous browser data replacing a valid paid snapshot.
- Added guided Browser Bridge setup for Chrome, Comet, Edge, and Brave, with registration support for Vivaldi, Opera, and Chromium.
- Prevented the Browser Bridge's Gemini token relay from colliding with Gemini Usage Monitor in the page's MAIN world.
- Added an NSIS installer and the user-facing **AI Quota Deck** shortcut name.
- Added provider, polling, Native Messaging, and Browser Bridge parser tests.

---
