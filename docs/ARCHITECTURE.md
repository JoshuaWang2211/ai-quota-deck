# AI Quota Deck — Architecture & Design Notes

This document captures **why** the app is built the way it is, including approaches
that were evaluated and rejected. Read it alongside the source to avoid
relitigating decisions or missing non-obvious constraints.

Every request shape and response sample below was **verified against live
accounts on 2026-08-08 or 2026-08-09**, and Grok Bot on **2026-08-28**, unless
explicitly marked otherwise. Where something is inference rather than
measurement, it says so.

> **Sample payloads in this document are redacted.** Real responses from Claude
> and Codex include `user_id`, `account_id`, and `email`. Never paste an
> unredacted response into this repo, an issue, or a bug report.

---

## 1. The central constraint: where the credential lives

This is the single fact that determines the app's shape.

| Provider | Credential location | Reachable from a desktop process? |
|----------|--------------------|-----------------------------------|
| Claude | `~/.claude/.credentials.json` (plaintext JSON on Windows) | **Yes** |
| Codex | `~/.codex/auth.json` (plaintext JSON) | **Yes** |
| Grok | Browser session (primary); `~/.grok/auth.json` (fallback) | **Both** |
| Gemini | `gemini.google.com` cookie **+ a token that only exists in a rendered page** | No |
| Antigravity | `--csrf_token` on the IDE's language-server command line (process-local) | **Yes**, while the IDE runs |
| Grok Bot | Chromium-encrypted short-lived access token in `%APPDATA%\Grok Bot` | **Yes**, through Windows DPAPI |

The pattern is that a vendor's **CLI or desktop client** leaves an OAuth token in
the user's home directory, and that token is enough to read the account's quota.
Four of six do this. Gemini has no such client, so it is the hard exception
(§6), and Antigravity keeps its credential inside a running process instead of
on disk (§7). Grok deliberately uses both routes: browser data avoids the CLI's
six-hour token expiry, while the disk token still works when no browser tab is
open (§5).

```
Chrome                                  Desktop
┌────────────────────────┐             ┌──────────────────────────────┐
│ Grok / Gemini extension│             │ ai-quota-deck (Tauri)        │
└────────────────────────┘             │   tray icon + dashboard      │
       │ connectNative                 │                              │
       ▼                               │   Claude poller ──→ HTTPS    │
┌────────────────────────┐   cache     │   Codex  poller ──→ HTTPS    │
│ ai-quota-deck.exe      │───────────→ │   Grok browser → CLI fallback│
│ (native-host mode)     │             │   Antigravity → local IDE    │
└────────────────────────┘             └──────────────────────────────┘
```

**Rule: ask where the credential lives before anything else.** On disk → poll it.
Browser-only → push it. That single question decides the whole integration, and
it is worth re-asking per provider rather than assuming: Grok was initially
classified as browser-bound because its usage was known only through a browser
extension, and that was wrong (§9).

### What this costs the user

Each direct provider requires a local sign-in: Claude Code for Claude, Codex
Desktop for Codex, a running Antigravity IDE for Antigravity, and optionally
Grok Build as Grok's fallback. Browser-backed
Gemini and Grok instead require the bundled Browser Bridge and a signed-in tab.
A subscription alone is not enough because the deck never asks users to paste a
credential.

Provider discovery is deliberately local and additive. A missing credential or
browser cache returns `not_configured` before any vendor request is attempted,
and the dashboard hides that provider. Existing integrations that are expired,
stale, rate-limited, or otherwise broken remain visible with an actionable
state. If no provider is found, the dashboard shows onboarding instead of five
error cards. The setup panel points Claude users to Claude Code, Codex users to
Codex Desktop, Antigravity users to the IDE, and browser users to the advanced
Browser Bridge setup (Grok can still use its CLI fallback). The production bridge installation is guided
unpacked loading (§9). Local discovery repeats on the normal poll so a new
sign-in appears without restarting the app.

The same panel lists every provider with a checkbox. An unticked provider is
not polled at all — polling is a question of politeness and rate limiting (§2),
and a card nobody is looking at should not spend Claude's 429 budget. Its last
results, its retry schedule, and Claude's persisted cooldown are left untouched,
so re-enabling it shows the previous rows immediately and the next request still
passes every existing gate. The hide-list is stored as `hidden_providers` in
`widget.json` beside the view preferences, which is the one store the
dashboard, the companion window, and the tray already share; an absent field
means everything is visible, so older preference files and newly added
providers need no migration.

### Why Widget and Strip share a second window, but not a second poller

Widget and Strip reuse one frameless, always-on-top Tauri window that skips the
taskbar. Widget presents a narrow vertical table; Strip turns the same data into
a compact horizontal bar. Both can be dragged anywhere and remember independent
screen positions. Widget's lock blocks movement while leaving its controls
available, so the same button can unlock it. Strip remains freely movable. Its
optional pin button enables a Windows-only Taskbar overlay without registering
an AppBar, so it never reserves or changes the Windows work area.

The companion window never calls a provider command. The hidden main dashboard
remains the sole scheduler and sends quota-only snapshots through targeted Tauri
events. A native active-only tick prevents a due three-minute cycle from being
stranded by a throttled hidden WebView; request floors and backoff remain in
control. Neither companion view adds Claude, Codex, Antigravity, Gemini, or Grok
requests.

Selected view, Widget lock state, Taskbar overlay opt-in, and both sets of
physical screen coordinates are stored in
`%LOCALAPPDATA%\ai-quota-deck\widget.json`. The three views are Dashboard,
Widget, and Strip, stored as the existing `visible` + `strip` pair so older
preference files keep working. Only native drags may replace those coordinates;
moves caused by resize, sleep, DPI, monitor topology changes, or Taskbar overlay
alignment are ignored. After wake, the native activity watcher reasserts
always-on-top state and retries the saved position while monitors settle. If a
saved monitor is unavailable, the window can fall back to the primary display
without erasing the original coordinates.

While the Taskbar overlay is active, a Windows-native watcher enumerates primary
and secondary taskbars and aligns the Strip inside the nearest one without
writing `widget.json`. The saved long-axis coordinate is the last place the
user dragged; after a restart the watcher snaps again from that point. It
changes z-order only after Explorer has moved that taskbar above the Strip,
using `HWND_TOPMOST` with `SWP_NOACTIVATE`. Ordinary and maximized apps do not
take precedence over the taskbar Strip. A monitor-covering foreground window is
treated as full-screen only when its native z-order is also above that
monitor's taskbar; true full-screen apps and an auto-hidden taskbar temporarily
hide the Strip HWND. The saved view stays Strip, so the tray checkbox still
means "turn Strip off" rather than "restore a hidden window". Dragging remains
available, and releasing on another monitor makes that monitor's taskbar the
new target.

Content measurement may change Widget or Strip dimensions after a quota
snapshot, but a resize never runs monitor placement once that view has saved
coordinates. This is important on mixed-DPI desktops, where a physical window
position and a temporarily virtualized monitor rectangle can use different
scales; treating that mismatch as a disconnected display would snap the view to
the default top-right position even though the saved coordinates were correct.

### Why the current bridge is a separate process, but not a second executable

Chrome's native messaging **starts the host process itself**; an extension
cannot connect to an already-running application. Chrome therefore launches a
second process, but it reuses `ai-quota-deck.exe`: when the first argument is an
allowed `chrome-extension://.../` origin, the binary enters native-host mode
instead of starting Tauri. This avoids packaging and updating a second exe.

The host reads Chrome's 32-bit native-endian length-prefixed JSON from stdin,
validates that the origin may write that provider, and stores quota-only JSON
under `%LOCALAPPDATA%\ai-quota-deck\browser-cache`. The tray process reads a
fresh cache entry on its normal poll. A file cache was chosen over a named pipe
because it works whether the tray app or browser starts first, survives service
worker suspension, and contains no credential.

Registration lives at
`HKCU\Software\Google\Chrome\NativeMessagingHosts\<host-name>`, pointing at a
generated manifest whose `allowed_origins` contains the Deck companion ID plus
the two legacy dedicated-extension IDs. The companion origin may write Gemini
and Grok; each legacy origin remains restricted to its own provider. The app
refreshes this manifest and registry value on startup so the path follows
installed updates.

---

## 2. Rules that apply to every provider

**Never spend a refresh token.** Re-read the credential file on every poll and
let the vendor's own CLI or app own renewal. Refresh tokens rotate: spending one
without writing the replacement back would break the user's *primary tool*, not
just this dashboard. The one automated recovery path still respects this rule:
after a Claude `401`, the deck runs the official `claude update` command and
accepts only a changed access token written by Claude Code itself.

**Never hardcode a window period.** See §8. This is the most likely source of a
wrong-but-plausible number.

**Reading a quota does not consume it.** Measured: three consecutive calls to the
Codex usage endpoint left `used_percent` at 2, matching what the Codex client
itself recorded at the same moment. Poll intervals are a question of politeness
and rate limiting, not cost.

Polling is provider-specific. Local and browser-backed reads use the deck's
three-minute cycle. Claude uses a six-minute request floor and only polls while
the workstation is in use: native `GetLastInputInfo` / `OpenInputDesktop` probes
pause its network requests after five idle minutes or immediately on lock. A
native background watcher notices the user's return, but the request waits two
minutes so Windows networking, Claude Desktop, and Claude Code can settle first.
The same native watcher emits an active-only refresh tick every minute. It
recovers an overdue global cycle when the hidden WebView's timers are throttled,
while also letting an expired Claude 429 recover independently between cycles;
request floors and backoff suppress unnecessary calls. An overdue WebView timer
and a focus/reveal event apply the same grace period. No input contents or
activity history are read or stored.

Claude 429 responses retain their cooldown even when cached rows keep the card
in the `ok` visual state. An integer `Retry-After` is kept in full (up to a
24-hour corrupt-value guard) and receives a five-second boundary buffer.
Without a usable header, repeated 429s back off for 6, 12, 24, 48, then 60
minutes. A quota-only
`%LOCALAPPDATA%\ai-quota-deck\provider-cache\claude-rate-limit.json` stores the
absolute deadline, failure count, last real attempt, policy version, and
credential-file generation. The Rust backend serializes Claude fetches and
rechecks both the cooldown and six-minute request floor after taking the lock,
so WebView reloads, focus events, and App restarts cannot create an early
request. The dashboard scheduler honors `retry_after_seconds` as given and does
not keep a second backoff table. A changed Claude Code credential generation
clears only the old credential's gate.

Builds that capped `Retry-After` at 15 minutes are migrated once: a still-future
deadline is kept, the inflated failure count is dropped, and the new policy
takes over. A successful response clears the breaker and is stored as a
quota-only snapshot. If a later live request fails, unexpired rows remain
visible as cached for at most 24 hours, with the reason and retry countdown
still visible. If the six-minute floor is still running and no snapshot exists,
the next live check is allowed rather than inventing an error card.

**Credentials stay in memory.** No token value may reach a log line, an error
message, or a crash report.

---

## 3. Claude

```
GET https://api.anthropic.com/api/oauth/usage
Authorization: Bearer <accessToken>
```

Token: `~/.claude/.credentials.json` → `claudeAiOauth.accessToken`. `CLAUDE_CONFIG_DIR`
overrides the directory, as it does for Claude Code itself.

The request identity follows the currently installed Claude Code client:

```
Authorization: Bearer <accessToken>
Content-Type: application/json
User-Agent: claude-cli/<installed CLI version> (external, cli)
x-app: cli
anthropic-version: 2023-06-01
anthropic-beta: oauth-2025-04-20
```

The CLI version is read once per process via `claude --version`, with a fixed
fallback only when no executable can be discovered. An earlier differential
test found that bare `Authorization` received a 200 response, but that proved
only temporary acceptance, not equivalent rate-limit classification. The full
client identity is therefore treated as part of the endpoint contract.

### Rejected access tokens recover through Claude Code

On `401`, the deck runs `claude update` invisibly and waits up to 60 seconds.
It does not inspect command output and never reads the OAuth refresh token. If
Claude Code replaces the rejected access token in `.credentials.json`, the deck
rereads the file and retries the usage request once in the same refresh cycle.
`claude update` may also replace the installed CLI, so an updater that outlives
the wait is left to finish rather than killed mid-replace, and a second one is
never started while the first still runs. If the CLI is unavailable, the token
does not change, or the retry is rejected again, cached quota remains visible,
the UI asks the user to open Claude Code, and the automatic path stays quiet
for an hour before it may try again.

> On macOS the credential lives in the Keychain instead. Out of scope; this is a
> Windows app.

### The plan comes from a second endpoint

```
GET https://api.anthropic.com/api/oauth/profile
Authorization: Bearer <accessToken>
```

⚠️ **Do not read `subscriptionType` from `.credentials.json`.** On a Claude Max
5x account it reads `pro`, and the neighbouring `rateLimitTier` reads
`default_claude_ai`. Both are wrong. The usage response carries no plan field at
all, so the profile endpoint is the only reliable source:

```json
{ "account":      { "has_claude_max": true, "has_claude_pro": false },
  "organization": { "organization_type": "claude_max",
                    "rate_limit_tier": "default_claude_max_5x",
                    "subscription_status": "active" } }
```

The label is assembled from both fields the provider gives: `organization_type`
names the tier, and a trailing `<digits>x` on `rate_limit_tier` supplies the
multiplier — `claude_max` + `default_claude_max_5x` → **Max 5x**. An unfamiliar
tier keeps its own words rather than being mapped onto a known one.

Cached for six hours (thirty minutes after a failed lookup). A plan changes when
someone upgrades and at no other time, and this endpoint family answers 429 when
pushed. The lookup runs only after the usage call has already succeeded, so a
throttled account never gets a second request piled on, and a profile failure
only costs the label — never the reading.

### Response

```json
{ "five_hour":  { "utilization": 40.0, "resets_at": "2026-01-01T00:00:00Z" },
  "seven_day":  { "utilization": 10.0, "resets_at": "2026-01-07T00:00:00Z" },
  "seven_day_opus": null, "seven_day_sonnet": null, "seven_day_cowork": null,
  "extra_usage": { "is_enabled": false, "...": "..." },
  "spend": { "percent": 0, "severity": "normal", "...": "..." },
  "limits": [
    { "kind": "session",       "group": "session", "percent": 40, "severity": "normal",
      "resets_at": "2026-01-01T00:00:00Z", "scope": null, "is_active": true },
    { "kind": "weekly_all",    "group": "weekly",  "percent": 10, "severity": "normal" },
    { "kind": "weekly_scoped", "group": "weekly",  "percent": 10,
      "scope": { "model": { "display_name": "..." } } } ] }
```

**Parse `limits[]`, not the flat fields.** `five_hour` / `seven_day_opus` /
`seven_day_cowork` are the older interface and are `null` on plans that don't
have those buckets — a `null` there means "not applicable", not "broken". The
array is self-describing: `kind`, `group`, `percent`, `severity`, and an optional
`scope` naming the model the bucket applies to.

`severity` can drive the colour scale directly instead of the app inventing its
own thresholds.

---

## 4. Codex

```
GET https://chatgpt.com/backend-api/wham/usage
Authorization: Bearer <access_token>
```

Token: `~/.codex/auth.json` → `tokens.access_token`. A JWT issued by
`auth.openai.com`, ten-day lifetime, rotated into that file by the Codex app.

`wham` is OpenAI's internal codename for the Codex backend. Three path variants
appear in the shipped `codex.exe` binary; only two work:

| URL | Result |
|-----|--------|
| `/backend-api/wham/usage` | 200 |
| `/backend-api/codex/usage` | 200, byte-identical response |
| `/backend-api/api/codex/usage` | **404** |

The `/api/codex/...` strings in the binary belong to a different base URL. Don't
assemble them against `chatgpt.com/backend-api`.

`Authorization` is the only required header. `chatgpt-account-id` and
`originator` are accepted but optional (verified: 200 with and without each).

### Response

```json
{ "plan_type": "free",
  "rate_limit": {
    "allowed": true, "limit_reached": false,
    "primary_window":   { "used_percent": 2, "limit_window_seconds": 2592000,
                          "reset_after_seconds": 2591416, "reset_at": 1788795325 },
    "secondary_window": null },
  "credits": { "has_credits": false, "unlimited": false, "balance": null },
  "spend_control": { "reached": false, "individual_limit": null },
  "additional_rate_limits": null, "code_review_rate_limit": null,
  "rate_limit_reached_type": null,
  "rate_limit_reset_credits": { "available_count": 0, "applicable_available_count": 0 } }
```

> **Plus was measured on 2026-08-09.** It returned `plan_type: "plus"` and one
> `primary_window` with `limit_window_seconds: 604800` (Weekly).
> `secondary_window`, `additional_rate_limits`, and `code_review_rate_limit`
> were all `null`. A paid plan therefore does **not** imply 5-hour + weekly;
> slots remain optional and must be labelled from their reported duration.

### Offline fallback

Codex writes the same numbers into its conversation logs:
`~/.codex/sessions/<yyyy>/<mm>/<dd>/rollout-*.jsonl`, one JSON object per line.
Look for `type == "event_msg"` with `payload.type == "token_count"`, then read
`payload.rate_limits`:

```json
{ "limit_id": "codex", "limit_name": null,
  "primary": { "used_percent": 2.0, "window_minutes": 43200, "resets_at": 1788795325 },
  "secondary": null, "plan_type": "free",
  "credits": { "has_credits": false, "unlimited": false, "balance": null },
  "individual_limit": null, "spend_control_reached": null,
  "rate_limit_reached_type": null }
```

**The field names differ from the HTTP response** — `primary` vs
`primary_window`, `resets_at` vs `reset_at`, `window_minutes` vs
`limit_window_seconds`. Same underlying data, two serializations; the values were
confirmed identical when read from both sources at the same moment.

Limitations that make this a fallback and not a primary source:

- It only advances after a Codex turn completes. Anything done on the web never
  reaches local files at all.
- Older Codex versions don't write the field. Of three session files on the
  development machine, only the newest contained `rate_limits`; the two from
  2026-05 had `token_count` events with no such key.

When serving fallback data the UI must say how old it is, not present it as live.

### `logs_2.sqlite`: evaluated, does not work here

`~/.codex/logs_2.sqlite` has a `logs` table, and rows with
`target = 'codex_api::endpoint::responses_websocket'` can carry
`{"type":"codex.rate_limits"}` websocket events — a fresher source than the JSONL
in principle.

Measured on the development machine (Codex app 26.803.41515): that target had 6
rows and **none** contained `rate_limits`; a full-table search for the string
returned zero. Either a version difference or a logging-level difference. Not
built against.

---

## 5. Grok

Grok is deliberately dual-source. A successful extension fetch is pushed
through Native Messaging and wins while its cache is no more than five minutes
old. If that snapshot is older, the deck tries the CLI. When the CLI is
unavailable, fails, or needs user action, a browser snapshot may still be shown
as stale for up to 24 hours. A paid snapshot is invalid immediately after its
reported reset time. The extension reads both account types from the source
Grok itself uses:

| Account | Browser source | Deck rendering |
|---------|----------------|----------------|
| Paid | gRPC-Web `GetGrokCreditsConfig` | Weekly used percent, reset, Chat / Voice / Build / Imagine breakdown |
| Free | `POST /rest/rate-limits` | Per-model query counts and used percent |

Unauthorized, empty, future-dated beyond one minute, and snapshots older than
24 hours are ignored. The effective order is fresh browser, CLI, then reusable
stale browser. Multiple browsers and profiles currently share one provider
cache, so the last successful writer selects the account displayed.

⚠️ **A signed-out tab is indistinguishable from a free-plan tab.**
`/rest/subscriptions` answers an anonymous session with `200` and an empty
subscription array, exactly as it answers a signed-in free account, so the
companion pushes the anonymous query allowance in both cases. Measured on a
signed-out grok.com tab: `{"paid":null,"unauthorized":false,"buckets":[{"label":
"Fast","remaining":2,"total":2}]}`. Nothing the extension can read separates the
two — it has no `cookies` permission, and no other endpoint has been verified.

Two guards follow from that, because a paid account must not be redrawn as a
free one by a tab the user forgot to sign in to:

1. **The native host refuses a free-only push while a usable paid snapshot is on
   disk** (`free_push_would_bury_paid`). Bounded: once that snapshot passes 24
   hours or the reset it reported, free data lands, so a real downgrade still
   takes effect. This guard is browser-only and does not depend on the CLI.
2. **Grok Build is sold only to paid accounts**, so `~/.grok/auth.json` existing
   is itself evidence of one. Fresh browser data reporting free-tier counts
   contradicts that credential, and the deck asks the CLI instead, falling back
   to the browser figure only if the CLI cannot answer.

The first guard carries the design: many Grok subscribers never install Grok
Build, so correctness cannot rest on a credential being present.

### CLI fallback

```
GET https://cli-chat-proxy.grok.com/v1/billing?format=credits
Authorization: Bearer <key>
```

Token: `~/.grok/auth.json`, written by Grok Build. The file is keyed by
`"<oidc_issuer>::<client_id>"`, so read entries by value rather than a fixed key
path; the token itself is the `key` field. Signing in to a second account adds a
second entry: the deck prefers a still-usable credential — an expired one can
never serve a request, and a missing `expires_at` means "not known to be
expired", not "oldest" — then the furthest expiry among those. A JWT from
`https://auth.x.ai` with a **six-hour** lifetime — much shorter than Claude's or
Codex's, so a stale-token path will be exercised often. `expires_at` sits
alongside it.

The CLI is not a daemon. It refreshes only while it is running, shortly before
expiry or after a 401. The deck must never spend the `refresh_token`: rotation
could invalidate the CLI's own login. If no fresh or reusable stale browser
data exists and the access token is expired, the card enters `action_required`
and asks the user to open Grok once; it does not show retry backoff for a
condition retries cannot repair.

`Authorization` is the only required header (verified against `x-userid` and
`x-grok-client-mode`, both optional). Base URL is overridable via
`GROK_CLI_CHAT_PROXY_BASE_URL`.

### The `format` parameter selects a different quota entirely

| Request | Returns |
|---------|---------|
| `?format=credits` | The **subscription** window — what SuperGrok users care about |
| `?format=rate_limits` *(also the default)* | The **xAI API console** credit balance — a different product |

Getting this wrong yields a confident, well-formed, completely unrelated number.
On the development account the two read 44 % and 13.7 % at the same moment.

### Response (`format=credits`)

```json
{ "config": {
    "currentPeriod": { "type": "USAGE_PERIOD_TYPE_WEEKLY",
                       "start": "2026-01-01T00:00:00+00:00",
                       "end":   "2026-01-08T00:00:00+00:00" },
    "creditUsagePercent": 44.0,
    "productUsage": [ { "product": "GrokChat",    "usagePercent": 33.0 },
                      { "product": "GrokVoice",   "usagePercent":  5.0 },
                      { "product": "GrokBuild",   "usagePercent":  3.0 },
                      { "product": "GrokImagine", "usagePercent":  3.0 } ],
    "onDemandCap": { "val": 0 }, "onDemandUsed": { "val": 0 },
    "prepaidBalance": { "val": 0 }, "isUnifiedBillingUser": true,
    "topUpMethod": "TOP_UP_METHOD_SAVED_PAYMENT_METHOD",
    "billingPeriodStart": "...", "billingPeriodEnd": "..." } }
```

`currentPeriod.type` names the window (`USAGE_PERIOD_TYPE_WEEKLY` observed) —
another provider declaring its own period rather than leaving it to be inferred
(§8).

`productUsage` is a per-product split of one subscription — chat, voice, coding,
and image generation drawing on the same pool. Both the CLI and browser sources
carry this breakdown, so switching sources does not remove rows from the card.

Cross-checked against Grok Build's own `/usage` screen: `creditUsagePercent`
44.0 matched its "Weekly limit: 44%", and `currentPeriod.end` matched its stated
reset time exactly.

> **Free Grok has no subscription window** — it is limited by per-model query
> counts, which the CLI billing endpoint does not describe. The extension path
> has been verified against a free account and supplies those counters instead.

The companion refreshes Grok every three minutes even while its tab is hidden.
The schedule belongs to the MV3 service worker's `chrome.alarms`, which messages
all open provider tabs; ordinary content-script intervals proved unreliable in
Comet even after removing their visibility guard. Matching provider tabs are
marked non-auto-discardable. If Chromium nevertheless reports one frozen or
discarded, the service worker reloads it; returning from an idle or locked system
state also triggers an immediate refresh. Browser or tab closure still stops
collection; the CLI and reusable snapshot remain the fallback in that case.

---

## 6. Gemini

No usable endpoint. The extension reads `window.WIZ_global_data.SNlM0e` — Google's
session-scoped XSRF token — from the **MAIN world** of a loaded page, then calls
an internal `batchexecute` RPC with it.

**That token is produced during page render and exists nowhere on disk.** No
amount of cookie access substitutes for it. Gemini is therefore push-only *and*
requires an open `gemini.google.com` tab. While that tab remains loaded, the
companion's MV3 alarm asks the tab to refresh and push quota every three minutes
even when it is in the background. The bridge opts matching tabs out of automatic
discarding and reloads one already reported as frozen or discarded. Closing the
browser or every matching tab still freezes the numbers at whatever arrived last.
The dashboard must show the age rather than the illusion of a live figure.

### Deck companion collector

`browser-bridge/` is a dedicated, UI-free companion extension. It does not read,
modify, or depend on the sibling `gemini-usage-monitor` extension. Its MAIN-world
interceptor exposes the page's WIZ token only to its own isolated content script;
that script makes the same-origin quota request, parses both windows, and stores
an account-namespaced 25-second deduplication cache. A three-minute
`chrome.alarms` schedule in the service worker messages open provider tabs; this
replaced content-script intervals after hidden-tab timers proved unreliable in
Comet. `runtime.onStartup` recreates that alarm when a browser restart does not
preserve it. Cookies and the WIZ token never cross Native Messaging.

The companion's service worker is the only code allowed to call
`chrome.runtime.connectNative`. It validates provider, origin, and main frame
before forwarding quota-only JSON. One manifest public key fixes the unpacked
extension ID at `alckoeangnmpomfnafaajjbpniomhnke`, so the native-host allowlist
is stable across Chromium profiles. The optional `nativeMessaging` permission
is requested only from a toolbar click. The non-warning `idle` permission exposes
only `active` / `idle` / `locked`; the bridge uses the transition back to active
as a recovery trigger and records no activity history.

The Gemini token relay runs in the page's MAIN world because `WIZ_global_data`
is not visible from an isolated content script. Its declarations must remain
inside a private function scope: MAIN-world scripts from different extensions
share the page's global lexical environment, and Gemini Usage Monitor uses some
of the same ordinary constant names. A top-level redeclaration would prevent the
later interceptor from executing at all.

Real Chrome E2E delivered account `u0`, tier 2, 5-hour usage 0% (2400 remaining),
and weekly usage 8.447195% (44296 remaining), including both reset timestamps.
The same companion also delivered a live SuperGrok weekly snapshot and product
breakdown without changing either sibling extension repository.

The two existing Chrome Web Store extensions remain independent products and are
not transports for the Deck. The dedicated Browser Bridge is bundled with the
app and installed through the guided unpacked flow in §9.

### Payload shape

The companion transports this payload:

```js
{ account_id, tier,
  remaining5h, ratio5h, resetTime5h,
  remaining7d, ratio7d, resetTime7d }
```

Three traps in that structure:

- **`ratio` is the fraction *used* (0–1); `remaining` is the count *left*.** They
  run in opposite directions. The extension renders `Math.round(ratio * 100)` as
  the usage percentage.
- **`remaining5h` and `remaining7d` are in different internal units** (2400 vs
  48384 on Pro). Never compare, sum, or plot them on one scale. This is why the
  sibling extension shows percentages only.
- **Storage keys are namespaced per Google account** — the companion uses an
  `_u3` suffix for a tab on `/u/3/`, and carries the account id in every push.
- **Tier code 1 is Free and 2 is Google AI Pro.** Unknown codes render as
  `Account N` instead of being assigned a guessed subscription name.

A snapshot is live for five minutes and then visibly cached. Each row disappears
after its own reset timestamp; the cache is never used beyond one weekly window.
As with Grok, the current cache holds one provider snapshot, so the last pushed
account wins.

---

## 7. Antigravity

Antigravity is a third answer to the §1 question. The credential is neither on
disk nor in a browser: it is a per-process CSRF token that exists only on the
command line of the language server the IDE starts, and it changes on every
launch. A desktop process owned by the same user can read it, so Antigravity is
polled — but only while the IDE is running.

```
POST https://127.0.0.1:<port>/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary
Content-Type: application/json
Connect-Protocol-Version: 1
X-Codeium-Csrf-Token: <csrf_token>

{}
```

**Verified on 2026-08-21** against Antigravity IDE `1.107.0` (`ideVersion 2.5.5`)
on a Google AI Pro account.

### Finding the server

The IDE spawns `language_server_windows_x64.exe` (`language_server_windows_arm.exe`
on ARM) with, among others, `--csrf_token <uuid>` and `--app_data_dir antigravity-ide`.
Two traps:

- `--extension_server_port` is **not** the API port. It belongs to the IDE side.
  The API port is not on the command line at all; the deck enumerates the
  process's listening TCP ports and tries them in ascending order.
- The line also carries `--extension_server_csrf_token`. Only the literal
  `--csrf_token` flag (space or `=` form) is the right token.

One IDE runs more than one language server: a global one owned by the main IDE
process, and one per workspace window carrying `--enable_lsp`, `--workspace_id`
and `--parent_pipe_path`. Both answer the same quota for the same account, so
the deck prefers the process without `--enable_lsp` and falls back to the rest.
Each process listens on two loopback ports: the lower one speaks TLS with a
certificate bundled in the binary (`CN=localhost`, SAN `127.0.0.1`, self-signed),
the next one is a cleartext twin of the same API. The LSP-enabled process adds
a third socket that is not the Connect API. The deck speaks HTTPS only, through
a client built inside `antigravity.rs` that accepts the self-signed certificate
and can only ever be pointed at `127.0.0.1`.

Discovery uses Win32 directly — a Toolhelp32 snapshot, the process's PEB for
its command line, and `GetExtendedTcpTable` for its listeners — and repeats on
every poll, so a restarted IDE (new token, new ports) is picked up on the next
cycle without any stored state. Measured cost with 408 processes on the
development machine: about 10 ms and no child process. Every third-party tool
surveyed shells out to PowerShell or `wmic` instead; their Windows bug reports
(quoting, antivirus prompts, warm-up delays, `wmic` missing on Windows 11 24H2)
all come from that choice. `RetrieveUserQuota` answers 404 and
`GetCommandModelConfigs` 501 on this build; `GetUnleashData` answers 200 and is
what the community extensions use as a port probe. The deck probes with the
summary call itself.

### Response

```json
{ "response": { "groups": [
    { "displayName": "Gemini Models",
      "description": "Models within this group: Gemini Flash, Gemini Pro",
      "buckets": [
        { "bucketId": "gemini-weekly", "displayName": "Weekly Limit Remaining",
          "window": "weekly", "remainingFraction": 0.99684095,
          "resetTime": "2026-08-24T02:50:47Z" },
        { "bucketId": "gemini-5h", "displayName": "Five Hour Limit Remaining",
          "window": "5h", "remainingFraction": 1,
          "resetTime": "2026-08-21T18:06:34Z" } ] },
    { "displayName": "Claude and GPT models",
      "description": "Models within this group: Claude Opus, Claude Sonnet, GPT-OSS",
      "buckets": [
        { "bucketId": "3p-weekly", "window": "weekly", "remainingFraction": 1,
          "resetTime": "2026-08-28T13:06:34Z" },
        { "bucketId": "3p-5h", "window": "5h", "remainingFraction": 1,
          "resetTime": "2026-08-21T18:06:34Z" } ] } ],
  "description": "Within each group, models share a weekly limit and a 5-hour limit. ..." } }
```

Two independent pools, each with a weekly and a five-hour bucket. The payload
names its own window (`window`), so the label comes from there — `"5h"` is
*Session (5h)*, `"weekly"` is *Weekly*, anything else keeps its own word and
gets no pace marker (§8). The group name becomes the scope: *Weekly · Gemini*,
*Session (5h) · Claude+GPT*. Three traps:

- **`remainingFraction` is the fraction *left*, 0–1.** Used percent is
  `(1 − remainingFraction) × 100`.
- **An untouched bucket carries a sliding `resetTime`.** While
  `remainingFraction` is exactly `1`, `resetTime` is simply the server's last
  refresh instant plus the window length, and it moves every few minutes. It is
  not a deadline. The deck drops `resets_at` for a full bucket and trusts the
  value only once something has been consumed — the partially used weekly
  bucket above kept a fixed `resetTime` that matched its own description
  ("fully refresh in 2 days, 13 hours").
- **The response contains no account identity**, which is why it is the only
  Antigravity payload the deck stores. `GetUserStatus` does carry `name` and
  `email`; the deck reads it through a typed struct that declares only
  `userTier.name` (the Google plan, *Google AI Pro*) and, as a fallback,
  `planStatus.planInfo.planName`, caches that label in memory for six hours,
  and never writes the payload anywhere. Its `planStatus.availablePromptCredits`
  / `monthlyPromptCredits` pair (500 of 50 000 on an idle account) is a legacy
  field from the server's Codeium lineage and would render as 99 % used; it is
  not an Antigravity quota and is never shown.

`GetUserStatus` also lists every model with a `quotaInfo` of its own, but all
fourteen entries repeat the same pool value, the array order changes between
calls, and no window is named. The community extensions built on that shape
before the summary call existed; the deck does not use it.

### States

The CSRF token is the credential, and it does not exist while the IDE is
closed. The deck's own quota-only snapshot,
`%LOCALAPPDATA%\ai-quota-deck\provider-cache\antigravity.json`, decides what a
missing process means, exactly as the browser cache does for Gemini (§6):

| Situation | Card |
|-----------|------|
| No process, and the deck has never read Antigravity | `not_configured`; appears only in the setup panel |
| No process, snapshot under 24 hours with unexpired rows | `ok` shown as cached, reason "Antigravity IDE is not running" |
| No process, snapshot present but expired or unreadable | `action_required`: open the IDE once; no backoff, rechecked every cycle at ~10 ms |
| Process present but every port fails (401, timeout, TLS, bad JSON) | cached if a snapshot exists, otherwise `error` with the normal backoff |
| Process present but its command line cannot be read — an elevated IDE under a deck that is not | cached if a snapshot exists, otherwise `error` naming that cause |
| Summary answers 404 or 501 | `action_required`: this deck version needs a newer IDE |
| 200 with no usable bucket | `unavailable` |

Reading consumes nothing: repeated calls seconds apart returned byte-identical
numbers, and the server itself only refreshes from Google every few minutes, so
the deck's ordinary three-minute cycle is enough. There is no idle gating and no
request floor — nothing here reaches Google. A free plan is documented as
weekly-only, so fewer rows is normal, not `unavailable`.

Widget and Strip show the two weekly buckets only, for the same reason Grok
shows only its seven-day window: that is the one that locks an account out for
days, and four metrics per row would not fit. The Strip abbreviation is `AG`.

### Why not read the quota with the IDE closed

Every tool that does so spends a Google refresh token — lifted from the IDE's
`state.vscdb`, from the `agy` CLI's credential-store entry, or from a login flow
of its own — to call `cloudcode-pa.googleapis.com` while impersonating the
Antigravity client. That breaks the rule in §2, and it is also the exact surface
on which Google has been disabling accounts for Terms-of-Service violations
since February 2026. The only refresh-free variant, reusing the `agy` CLI's
stored access token as-is, goes stale about an hour after the CLI last ran and
still speaks to Google under a borrowed identity. Keeping the IDE open is the
same kind of requirement Gemini already imposes with its browser tab, and it
costs the user nothing they are not already doing when they use Antigravity.

### Grok Bot: a separate product and allowance

Grok Bot uses Grok models, but its allowance is metered independently from both
Grok/SuperGrok and Cursor. The Windows desktop app stores a Chromium-style
profile in `%APPDATA%\Grok Bot`: `Local State` contains a DPAPI-wrapped AES key,
and `sand-secrets.json` contains AES-256-GCM `v10` values for its machine id and
accounts. The deck selects the active account and deserialises only
`cursor-access-token`; the refresh-token field is intentionally absent from the
Rust data shape and is never decoded or decrypted.

The live call mirrors the app's Connect unary RPC:

```text
POST https://api2.cursor.sh/aiserver.v1.DashboardService/GetSandUsageStatus
Content-Type: application/proto
Connect-Protocol-Version: 1
```

The protobuf response supplies `usage_percent`, period start, next UTC reset,
availability flags, and a Grok plan label. The period endpoints determine
`window_seconds`; the card does not assume seven days merely because this is the
only row. Pooled enterprise allowances and zero included limits are
`unavailable`, because neither yields a meaningful individual percentage.

Only the normalized plan, percent, reset, and observation time enter
`%LOCALAPPDATA%\ai-quota-deck\provider-cache\grok-bot.json`. The snapshot expires
after 24 hours or at its reset, whichever comes first. An expired or rejected
access token is `action_required`: open Grok Bot so its own account machinery can
renew it. AI Quota Deck never spends the refresh token.

---

## 8. Window periods are slots, not fixed durations

The most dangerous assumption in this codebase.

Codex's `primary` and `secondary` are **slots**. What lands in them depends on
the plan. A free account's `primary` is a **30-day** window; labelling it
"5-hour" because it is first would be wrong and would look entirely plausible.

Codex's own status-line component offers four limit widgets — `five-hour-limit`,
`daily-limit`, `weekly-limit`, `monthly-limit` — so at least four periods exist.
Derive the label from the duration:

| `limit_window_seconds` | `window_minutes` | Label |
|-----------------------|------------------|-------|
| 18 000 | 300 | Session (5 hours) |
| 86 400 | 1 440 | Daily |
| 604 800 | 10 080 | Weekly |
| 2 592 000 | 43 200 | Monthly |

Claude solves the same problem differently: its `limits[]` entries carry `kind`
and `group`, so the label comes from the payload rather than from array position.
The known `session` and `weekly_*` kinds also map to 5 hours and 7 days for the
pace marker; an unknown kind receives no pace marker. Either way — **the provider
names the window, the app never guesses from its slot.**

The full dashboard renders every reported window with reset and pace details.
Widget and Strip reuse the same normalized data but compress it to period
labels and percentages; Grok intentionally shows only its seven-day window.
Strip further shortens provider titles to two letters — CL, CO, AG, GE, GR, GB — so the
bar stays narrow; the full name remains on hover. Compact values follow the
companion extensions' thresholds: green below 70%, amber from 70%, and red
from 90%.

---

## 9. Considered and rejected

### Reading Chrome's cookie database directly

**Pros**: no extension required; Grok would work with the browser closed.

**Cons**: three, any one of which is disqualifying.

1. Chrome 127+ App-Bound Encryption ties the cookie key to the Chrome binary, so
   a foreign process can no longer decrypt it with DPAPI alone.
2. Reading another application's cookie store is the defining behaviour of
   credential-stealing malware. An open-source tool that strangers are asked to
   download should not do it, and antivirus heuristics will treat it accordingly.
3. **It still would not fix Gemini** — that needs the in-page token, not the
   cookie.

**Decision**: use Chrome's official native messaging path instead.

### A fully offline design (local files only)

The most prominent tool in this space reads only local files and makes no vendor
API calls, on the stated grounds that staying offline avoids consuming the user's
tokens.

**That rationale does not hold** — reading a quota consumes nothing (§2). The
real advantages of offline are privacy, one fewer dependency, and never touching
a credential. The real cost is staleness: numbers lag until the CLI next runs,
and web activity is invisible.

**Decision**: poll live for Claude and Codex, keep the local JSONL as a labelled
fallback. Offline-only is available to anyone who prefers it, but it should be a
choice, not a limitation.

### Polling Grok as the sole source — superseded

The original plan routed Grok through `grok-usage-watch` and
`POST https://grok.com/rest/rate-limits` (cookie-authenticated, per-model),
because that was the only way its usage had ever been observed. Then Grok Build
turned out to store an OAuth token on disk like the other two (§5).

Polling was briefly chosen as the sole source after the CLI endpoint exposed
paid `productUsage`. Two later measurements overturned that choice: the CLI's
access token expires after about six hours and refreshes only while the CLI is
running, while the extension's paid gRPC-Web path exposes the same weekly pool
and product breakdown. The extension also covers free query limits that the CLI
billing endpoint cannot report.

**Decision**: fresh extension data first, CLI polling only when browser data is
unavailable. The deck never consumes the CLI refresh token.

The lesson generalises: *"the only way anyone has read this number so far"* is
not the same as *"the only way this number can be read."* Check for a first-party
CLI before concluding a provider is browser-bound.

### Shipping three providers before the extensions — superseded

This was the intended first release while the bridge appeared to serve Gemini
alone. Grok's six-hour CLI token and free-tier gap made the bridge useful to two
providers, and the release boundary changed: all four providers are completed
and tested before the public repository is created.

### Reusing the existing Chrome Web Store extensions as Deck transports — rejected

Adding optional `nativeMessaging` is technically possible and would not disable
existing users merely because an update declared an optional permission. That
does not make it an acceptable product change. The existing Gemini and Grok
extensions have a narrow purpose: showing quota in the provider page. Sending
that data to a separate desktop product adds a capability and permission which
may be judged unrelated to that single purpose. Chrome Web Store guidance says
unrelated functionality and excessive permissions should be separated, and
that user data may only be transmitted for the product's disclosed purpose:

- [Extensions quality guidelines](https://developer.chrome.com/docs/webstore/program-policies/quality-guidelines)
- [Use of permissions](https://developer.chrome.com/docs/webstore/program-policies/permissions/)
- [Limited use of user data](https://developer.chrome.com/docs/webstore/program-policies/limited-use)

**Decision**: do not modify or depend on either published extension for Deck
transport. Keep both sibling repositories unchanged rather than exposing
working products to a new review boundary.

### Delivering browser collection without the Chrome Web Store

**Decision: guided unpacked.** The WebView2 connector was rejected without
running the spike. Two routes were weighed:

| Route | Benefit | Cost and required proof |
|-------|---------|-------------------------|
| Guided unpacked `browser-bridge/` | Reuses the collector and Native Messaging path already proven in real Chrome and Comet sessions | One manual Developer mode + **Load unpacked** per browser profile; the app can open the management page, reveal the stable folder, copy its path, and detect versions, but cannot press Chrome's protected UI for the user |
| Deck-owned WebView2 connector | No CWS item and potentially no Developer mode; collector code ships and updates with the app | Users sign in again inside an app-owned browser profile; must prove Gemini and Grok login, persisted sessions after restart, hidden/background refresh, and a quota-only IPC surface that remote pages cannot expand |

Windows and macOS Chrome only support self-hosted extensions in managed
enterprise environments. Official Chrome also removed `--load-extension` from
branded builds starting in Chrome 137, so an ordinary installer cannot silently
turn the unpacked route into a normal install:

- [Chrome extension distribution](https://developer.chrome.com/docs/extensions/how-to/distribute)
- [Removal of `--load-extension`](https://groups.google.com/a/chromium.org/g/chromium-extensions/c/1-g8EFx2BBY)

WebView2 officially supports document-created script injection and web/native
messaging, which makes a contained feasibility spike credible. It still uses an
app-owned profile rather than the user's Chrome or Comet profile. Attaching to a
normal Chrome profile is not a shortcut: Chrome 136 stopped honoring remote
debugging switches for the default data directory, requiring a separate profile
anyway.

- [WebView2 API overview](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/overview-features-apis)
- [Chrome remote-debugging profile restriction](https://developer.chrome.com/blog/remote-debugging-port)

Two other approaches are not competitive. Reading Chrome extension storage
would rely on unsupported, profile-specific LevelDB files and only helps Gemini;
the current Grok extension does not persist quota. A userscript posting to a
loopback HTTP or WebSocket server adds a third-party script manager, pairing
secret, local listener, and larger attack surface than one unpacked extension.

WebView2 was rejected on risk ownership rather than on a failed experiment. Its
first gate — whether Gemini permits sign-in inside an embedded webview — is
Google's policy, not ours; a pass would be revocable at any time, with the app
breaking and no recourse. It also asks the user to sign in to Google again
inside the deck, puts that session in the deck's custody, and polls a signed-in
account from a hidden window. The worst case there is a flagged Google account,
which is far more costly than a Developer mode warning. This is recorded as a
judgement, not a measurement: nothing above was tested, and reopening the route
starts by testing that first gate.

**Production shape.** `bundle.resources` ships `browser-bridge/` inside the
installer; on startup the app stages it to
`%LOCALAPPDATA%\ai-quota-deck\browser-bridge`, keyed by an `.installed-version`
stamp, and exposes that path through the `bridge_dir` command so the setup panel
can offer **Copy path** and **Open folder**. A fixed staging path is required
because `targets: "all"` produces both MSI and NSIS bundles with different
install roots, and NSIS lets the user change its root again. Staging also makes
an app update a bridge update: Chrome re-reads an unpacked extension from disk
on browser restart.

Publishing `browser-bridge/` as its own Chrome Web Store item remains the
long-term answer and would remove the Developer mode requirement entirely. The
single-purpose objection that ruled out modifying the two published extensions
does not apply to a standalone item whose only purpose is the relay. It is
sequenced after the public repository exists, since review is likely to want a
reachable companion app.

---

## 10. Known unknowns

### Which plans have actually been seen

Every response shape in this document came from one developer's accounts. The
gaps are not oversights — they are tiers nobody has run this against yet.

| Provider | Plan | Status |
|----------|------|--------|
| Claude | Max 5x | **Measured.** Session + weekly + model-scoped weekly |
| Claude | Pro, Max 20x, Team | Unseen. `limits[]` is self-describing, so extra buckets should appear on their own |
| Codex | Free | **Measured.** A single 30-day window |
| Codex | Plus | **Measured.** A single weekly primary; secondary and additional limits null |
| Codex | Pro | Unseen |
| Grok | SuperGrok | **Measured.** Weekly window plus a per-product split |
| Grok | Free | **Measured through the extension.** Per-model query counts; no subscription window |
| Antigravity | Google AI Pro | **Measured.** Two pools (Gemini; Claude + GPT), each with a weekly and a five-hour bucket |
| Antigravity | Free, Ultra | Unseen. Free is documented as weekly-only; fewer buckets is expected, not an error |
| Grok Bot | SuperGrok-linked | **Measured.** One independent weekly window with plan label `SuperGrok` |

A plan with nothing to report is **not an error**. All providers route
"the call worked and there is no such quota" to an `Unavailable` card: stated
plainly, no retry escalation, and re-checked only every half hour, because the
answer changes when someone subscribes and at no other time. Expect most people
who install the deck to see at least one.

`Unavailable` is distinct from `not_configured`. The former means a configured
provider answered successfully but that account has no applicable quota, so the
card stays visible. The latter means there is no local credential or browser
cache to use, so no network request is made and the provider appears only in the
setup panel.

If your plan renders wrongly, the useful bug report is the raw response with
`user_id`, `account_id` and `email` removed.

### Still open

- **Codex Pro payloads** (§4). Plus is measured; Pro is not.
- **Whether `additional_rate_limits` entries** carry `limit_id` / `limit_name`
  per bucket. They remained null on the measured Plus response.
- **Browser/profile account selection.** A Chrome Free account and a Comet paid
  account have both completed the real native-messaging flow. They currently
  share one Grok cache, so whichever browser pushes last determines what the
  deck shows. Explicit account selection is not implemented.

---

## 11. Stability outlook

All six interfaces are undocumented and unversioned.

| Signal | Reading |
|--------|---------|
| Claude serves both `five_hour` and `limits[]` in one payload | Mid-migration; the flat fields are the legacy side |
| Codex answers on both `/wham/` and `/codex/` | Same — an alias kept for compatibility |
| Codex omits `rate_limits` from older session logs | The local format changed recently too |
| Gemini's RPC id is Closure-generated | Changes whenever the quota service is refactored |
| Antigravity's IDE server answers the summary call today while older builds only served per-model `GetUserStatus` | The local API surface moves with IDE releases; flags and process names can too |
| Grok Bot's desktop RPC and Chromium secret schema are app-internal | Either can move with a desktop app update; protobuf unknown fields are skipped |

Expect breakage on the order of "a few times a year, per provider". The design
consequence: **each provider must fail independently.** One dead endpoint shows
one dead card, never an empty dashboard.

---

## 12. When something breaks

1. **Claude shows an auth error** → automatic `claude update` already failed;
   open Claude Code once and complete sign-in. **Codex shows one** → open Codex
   Desktop and sign in. The deck never handles either provider's refresh token
   itself (§2).
2. **Codex card is stale but Claude is fine** → the usage endpoint changed.
   Re-check the three path variants in §4 before assuming anything deeper.
3. **A window is labelled with the wrong period** → something hardcoded a slot
   instead of reading the duration (§8). Check the label derivation first; the
   data is probably correct.
4. **Codex shows an "offline" badge that never clears** → `auth.json` is missing
   or unreadable and the app fell back to session JSONL. Confirm the file exists
   and that the Codex app has been run at least once.
5. **Grok needs attention** → neither a fresh browser snapshot nor a reusable
   snapshot from the last 24 hours was available, and the fallback CLI token was
   absent or expired. Confirm the Browser Bridge is enabled, click its toolbar
   icon once, and keep a signed-in `grok.com` tab open; or run Grok Build once.
6. **A browser-backed card never populates** → verify the registry key
   under `NativeMessagingHosts`, that the manifest's `allowed_origins` matches
   the installed extension ID, and that its path is the current
   `ai-quota-deck.exe`. Feeding a length-prefixed JSON frame to native-host mode
   isolates host/cache faults from extension faults.
7. **Gemini alone is frozen** → expected with no loaded `gemini.google.com` tab
   (§6). After waking or unlocking, allow one three-minute cycle. If the age still
   does not advance, confirm the tab is signed in and reload the companion and
   Gemini page. The card should show its age rather than claim the snapshot is live.
8. **Antigravity asks you to open the IDE** → expected when no
   `language_server_windows_*.exe` is running and the last snapshot is older than
   24 hours (§7). Start Antigravity IDE; the card recovers on the next cycle. If
   the IDE is running and the card shows an error instead, either the IDE was
   started elevated while the deck was not, or an IDE update changed the server's
   command line — check that the language server process still carries
   `--csrf_token` and an `--app_data_dir` beginning with `antigravity`.
9. **Grok Bot needs attention** → open Grok Bot once so it can renew its own
   short-lived access token. The deck deliberately never handles its refresh
   token. A recent unexpired quota-only snapshot remains visible for 24 hours.
