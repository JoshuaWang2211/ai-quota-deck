# AI Quota Deck — Architecture & Design Notes

This document captures **why** the app is built the way it is, including approaches
that were evaluated and rejected. Read it alongside the source to avoid
relitigating decisions or missing non-obvious constraints.

Every request shape and response sample below was **verified against live
accounts on 2026-08-08 or 2026-08-09** unless explicitly marked otherwise. Where something is
inference rather than measurement, it says so.

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

The pattern is that a vendor's **CLI or desktop client** leaves an OAuth token in
the user's home directory, and that token is enough to read the account's quota.
Three of four do this. Gemini has no such client, so it is the hard exception
(§6). Grok deliberately uses both routes: browser data avoids the CLI's
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
│ (native-host mode)     │             │                              │
└────────────────────────┘             └──────────────────────────────┘
```

**Rule: ask where the credential lives before anything else.** On disk → poll it.
Browser-only → push it. That single question decides the whole integration, and
it is worth re-asking per provider rather than assuming: Grok was initially
classified as browser-bound because its usage was known only through a browser
extension, and that was wrong (§8).

### What this costs the user

Each direct provider requires a local sign-in: Claude Code for Claude, Codex
Desktop for Codex, and optionally Grok Build as Grok's fallback. Browser-backed
Gemini and Grok instead require the bundled Browser Bridge and a signed-in tab.
A subscription alone is not enough because the deck never asks users to paste a
credential.

Provider discovery is deliberately local and additive. A missing credential or
browser cache returns `not_configured` before any vendor request is attempted,
and the dashboard hides that provider. Existing integrations that are expired,
stale, rate-limited, or otherwise broken remain visible with an actionable
state. If no provider is found, the dashboard shows onboarding instead of four
error cards. The setup panel points Claude users to Claude Code, Codex users to
Codex Desktop, and browser users to the advanced Browser Bridge setup (Grok can
still use its CLI fallback). The production bridge installation is guided
unpacked loading (§8). Local discovery repeats on the normal poll so a new
sign-in appears without restarting the app.

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

**Never refresh a token.** Re-read the credential file on every poll and let the
vendor's own CLI or app handle renewal. Refresh tokens rotate: spending one
without writing the replacement back would break the user's *primary tool*, not
just this dashboard. A stale token is a visible, recoverable failure; a consumed
one is not.

**Never hardcode a window period.** See §7. This is the most likely source of a
wrong-but-plausible number.

**Reading a quota does not consume it.** Measured: three consecutive calls to the
Codex usage endpoint left `used_percent` at 2, matching what the Codex client
itself recorded at the same moment. Poll intervals are a question of politeness
and rate limiting, not cost.

Polling is provider-specific. Local and browser-backed reads use the deck's
three-minute cycle. Claude follows that cycle only while the workstation is in
use: native `GetLastInputInfo` / `OpenInputDesktop` probes pause its network
requests after five idle minutes or immediately on lock, and a native background
watcher resumes the check within about five seconds after the user returns. A
focus/reveal check still has a two-minute success cooldown. No input contents or
activity history are read or stored.

Claude 429 responses retain their cooldown even when cached rows keep the card
in the `ok` visual state. An integer `Retry-After` is honored; otherwise repeated
429s back off for 3, 6, 12, then 15 minutes, with 15 minutes as the ceiling. A
quota-only `%LOCALAPPDATA%\ai-quota-deck\provider-cache\claude-rate-limit.json`
stores only the deadline and consecutive count, so restarting the app cannot
bypass or reset an active cooldown. A
successful response is persisted as a quota-only snapshot; if a later live
request fails, unexpired rows remain visible as cached for at most 24 hours.
This prevents a temporary 429 from turning the Claude card into an empty error
without carrying a row past its reset.

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

The request deliberately matches the working Claude Code-compatible monitor:

```
Authorization: Bearer <accessToken>
Content-Type: application/json
User-Agent: claude-code/<installed CLI version>
anthropic-beta: oauth-2025-04-20
```

The CLI version is read once per process via `claude --version`, with a fixed
fallback only when no executable can be discovered. An earlier differential
test found that bare `Authorization` received a 200 response, but that proved
only temporary acceptance, not equivalent rate-limit classification. A later
same-account, same-time A/B test had the header-complete reference monitor
succeed while the bare-header deck received 429; operationally the complete
request identity is therefore required.

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
`"<oidc_issuer>::<client_id>"`, so read the single entry rather than a fixed key
path; the token itself is the `key` field. A JWT from `https://auth.x.ai` with a
**six-hour** lifetime — much shorter than Claude's or Codex's, so a stale-token
path will be exercised often. `expires_at` sits alongside it.

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
(§7).

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
app and installed through the guided unpacked flow in §8.

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

## 7. Window periods are slots, not fixed durations

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
Mini mode reuses the same normalized data but compresses it to period labels and
percentages; Grok intentionally shows only its seven-day window there. Mini
values follow the companion extensions' thresholds: green below 70%, amber from
70%, and red from 90%. The selected layout is kept locally, and the native
window resizes to fit its rendered content.

---

## 8. Considered and rejected

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

## 9. Known unknowns

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

## 10. Stability outlook

All four interfaces are undocumented and unversioned.

| Signal | Reading |
|--------|---------|
| Claude serves both `five_hour` and `limits[]` in one payload | Mid-migration; the flat fields are the legacy side |
| Codex answers on both `/wham/` and `/codex/` | Same — an alias kept for compatibility |
| Codex omits `rate_limits` from older session logs | The local format changed recently too |
| Gemini's RPC id is Closure-generated | Changes whenever the quota service is refactored |

Expect breakage on the order of "a few times a year, per provider". The design
consequence: **each provider must fail independently.** One dead endpoint shows
one dead card, never an empty dashboard.

---

## 11. When something breaks

1. **Claude shows an auth error** → run Claude Code once and complete sign-in.
   **Codex shows one** → open Codex Desktop and sign in. The deck never refreshes
   either token itself (§2).
2. **Codex card is stale but Claude is fine** → the usage endpoint changed.
   Re-check the three path variants in §4 before assuming anything deeper.
3. **A window is labelled with the wrong period** → something hardcoded a slot
   instead of reading the duration (§7). Check the label derivation first; the
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
