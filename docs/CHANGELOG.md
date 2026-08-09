# 📜 Changelog – AI Quota Deck

All notable changes to AI Quota Deck are documented here.

---

## v0.1.1 - 2026-08-09

- Fixed reinstalling or updating the app without creating the **AI Quota Deck** desktop shortcut.

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
