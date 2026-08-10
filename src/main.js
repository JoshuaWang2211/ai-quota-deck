import {
  isUserAway,
  missedRefreshCycle,
  providerRetryDelay,
  resumeGraceDeadline,
} from "./polling.js";
const { invoke } = window.__TAURI__.core;
const { emitTo, listen } = window.__TAURI__.event;

const PROVIDERS = [
  {
    id: "claude",
    name: "Claude",
    command: "claude_quota",
    setup: "Run Claude Code and sign in once.",
    // Claude's undocumented endpoint is more sensitive than the other sources.
    // Poll conservatively while active and cap 429 backoff at 15 minutes.
    pollMs: 360_000,
    pauseWhileAway: true,
    rateLimitBackoffMs: [180_000, 360_000, 720_000, 900_000],
    maxRateLimitBackoffMs: 900_000,
  },
  {
    id: "codex",
    name: "Codex",
    command: "codex_quota",
    setup: "Open the Codex app and sign in once.",
  },
  {
    id: "gemini",
    name: "Gemini",
    command: "gemini_quota",
    setup: "Install the optional Browser Bridge, open Gemini, then click the extension icon once.",
    optional: true,
  },
  {
    id: "grok",
    name: "Grok",
    command: "grok_quota",
    setup: "Install the optional Browser Bridge (recommended), or sign in with Grok Build.",
    optional: true,
  },
];
const BROWSER_PROVIDERS = PROVIDERS.filter(({ id }) => id === "gemini" || id === "grok");

// Three minutes, matching the sibling Claude tray tool's default. Reading a
// quota costs no quota, so the only cost is request volume against four
// undocumented endpoints — and one of them answers 429 if pushed.
const POLL_MS = 180_000;

// Defensive floor for duplicate lifecycle events or future scheduler changes.
// The visible countdown is the only normal refresh trigger.
const MIN_GAP_MS = 20_000;

// Per provider, and escalating: a failure that repeats usually needs time, not
// another attempt. One rate-limited provider must never slow the others.
const BACKOFF_MS = [60_000, 120_000, 300_000];

// The reference Claude monitor stops all network traffic after five minutes
// without keyboard/mouse input, and immediately on workstation lock.
const IDLE_PAUSE_SECONDS = 5 * 60;

// Let Windows networking and provider apps settle before Claude checks resume.
// This avoids racing Claude Desktop/Code immediately after a long sleep.
const CLAUDE_RESUME_GRACE_MS = 60_000;

// "This account has no such quota" only changes if someone subscribes, so there
// is nothing to gain from asking every three minutes.
const UNAVAILABLE_RECHECK_MS = 1_800_000;

// A few points of slack before calling a window "ahead of pace", so a row does
// not flicker in and out of the warning as the clock ticks.
const PACE_TOLERANCE = 4;

const THEME_KEY = "quota-deck-theme";

const providersEl = document.getElementById("providers");
const providerControlsEl = document.getElementById("provider-controls");
const updatedEl = document.getElementById("updated");
const countdownEl = document.getElementById("countdown");
const widgetModeEl = document.getElementById("widget-mode");
const stripModeEl = document.getElementById("strip-mode");
const themeEl = document.getElementById("theme");

let widgetPreferences = { visible: false, locked: false, strip: false };

const nowSec = () => Math.floor(Date.now() / 1000);

// ── Theme ───────────────────────────────────────────────────────────────────

const darkQuery = matchMedia("(prefers-color-scheme: dark)");

themeEl.addEventListener("click", () => {
  const next = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
  localStorage.setItem(THEME_KEY, next);
  document.documentElement.dataset.theme = next;
  void emitTo("widget", "widget-theme-changed", next).catch(() => {});
});

// Follow the system only while the user has expressed no preference of their own.
darkQuery.addEventListener("change", (event) => {
  if (localStorage.getItem(THEME_KEY)) return;
  document.documentElement.dataset.theme = event.matches ? "dark" : "light";
  void emitTo("widget", "widget-theme-changed", document.documentElement.dataset.theme).catch(
    () => {},
  );
});

// ── Companion views ──────────────────────────────────────────────────────────

function applyWidgetPreferences(preferences) {
  widgetPreferences = { ...widgetPreferences, ...preferences };
  const widgetActive = widgetPreferences.visible && !widgetPreferences.strip;
  const stripActive = widgetPreferences.visible && widgetPreferences.strip;
  widgetModeEl.setAttribute("aria-pressed", widgetActive.toString());
  stripModeEl.setAttribute("aria-pressed", stripActive.toString());
  widgetModeEl.title = widgetActive ? "Return to dashboard" : "Show desktop widget";
  stripModeEl.title = stripActive ? "Return to dashboard" : "Show strip";
}

async function setDisplayMode(mode, source) {
  try {
    applyWidgetPreferences(await invoke("set_display_mode", { mode }));
  } catch (error) {
    source.title = `Could not change display mode: ${error}`;
  }
}

widgetModeEl.addEventListener("click", () => {
  const active = widgetPreferences.visible && !widgetPreferences.strip;
  void setDisplayMode(active ? "dashboard" : "widget", widgetModeEl);
});

stripModeEl.addEventListener("click", () => {
  const active = widgetPreferences.visible && widgetPreferences.strip;
  void setDisplayMode(active ? "dashboard" : "strip", stripModeEl);
});

// ── Formatting ──────────────────────────────────────────────────────────────

function duration(seconds) {
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days) return `${days}d ${hours}h`;
  if (hours) return `${hours}h ${minutes}m`;
  if (minutes) return `${minutes}m`;
  return `${seconds}s`;
}

function untilReset(resetsAt) {
  if (!resetsAt) return null;
  const left = resetsAt - nowSec();
  return left <= 0 ? "Resetting now" : `Resets in ${duration(left)}`;
}

function ago(unixSeconds) {
  const past = nowSec() - unixSeconds;
  return past < 60 ? "just now" : `${duration(past)} ago`;
}

function updateAge(date) {
  const seconds = Math.max(0, Math.floor((Date.now() - date.getTime()) / 1000));
  if (seconds < 60) return "just now";

  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min ago`;

  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} hr ago`;

  const days = Math.floor(hours / 24);
  return `${days} day${days === 1 ? "" : "s"} ago`;
}

/** The provider's own severity wins where it offers one — it knows what "close
 *  to the limit" means for a given plan better than a threshold guessed here.
 *  The 70/90 fallbacks match the sibling extensions. */
function meterVar(percent, severity) {
  if (severity === "critical" || severity === "severe") return "var(--crit)";
  if (severity === "warning") return "var(--warn)";
  if (severity === "normal") return percent >= 90 ? "var(--crit)" : "var(--ok)";
  if (percent >= 90) return "var(--crit)";
  if (percent >= 70) return "var(--warn)";
  return "var(--ok)";
}

/** How far through the window we are, as a percentage. Needs both a duration
 *  and an end; Claude reports no duration, so its rows get no pace mark. */
function pacePercent(window_) {
  const { window_seconds: length, resets_at: end } = window_;
  if (!length || !end) return null;
  const elapsed = (nowSec() - (end - length)) / length;
  return Math.min(100, Math.max(0, elapsed * 100));
}

// ── Rendering ───────────────────────────────────────────────────────────────

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function renderWindow(window_) {
  const percent = Math.min(100, Math.max(0, window_.percent));
  const pace = pacePercent(window_);

  const row = el("div", "row");
  row.style.setProperty("--meter", meterVar(window_.percent, window_.severity));
  row.style.setProperty("--fill", `${percent}%`);

  const head = el("div", "row-head");
  head.append(el("span", "row-label", window_.label));

  const figure = el("span", "row-figure", Math.round(window_.percent).toString());
  figure.append(el("span", "unit", "%"));
  head.append(figure);
  row.append(head);

  const track = el("div", "track");
  track.append(el("div", "track-fill"));
  if (pace !== null) {
    const mark = el("div", "track-pace");
    mark.style.setProperty("--pace-at", `${pace}%`);
    track.append(mark);
  }
  row.append(track);

  const reset = untilReset(window_.resets_at);
  if (reset) {
    const note = el("p", "row-note", reset);
    if (pace !== null && window_.percent > pace + PACE_TOLERANCE) {
      note.append(document.createTextNode(" · "));
      note.append(el("span", "ahead", "ahead of pace"));
    }
    row.append(note);
  }

  return row;
}

function renderProvider(provider, quota) {
  const section = el("section", "provider");

  const head = el("div", "provider-head");
  head.append(el("h2", "provider-name", provider.name));

  if (quota?.status === "ok" && quota.plan) {
    head.append(el("span", "plan", quota.plan));
  }
  if (quota?.status === "ok" && quota.stale) {
    head.append(el("span", "badge", "cached"));
  }
  section.append(head);

  if (!quota) {
    section.append(el("p", "note", "Reading…"));
    return section;
  }

  // Nothing is wrong here — the account just has no quota of this kind. Say it
  // plainly and quietly; most people who install the deck will see at least one.
  if (quota.status === "unavailable") {
    section.append(el("p", "note quiet", quota.message));
    return section;
  }

  // Another application owns this credential and must renew it. Treating this
  // as a transient error would promise retries that cannot possibly help.
  if (quota.status === "action_required") {
    section.append(el("p", "note", quota.message));
    return section;
  }

  if (quota.status === "error") {
    // The Rust side phrases these as something to do, not as a stack trace.
    const note = el("p", "note", quota.message);
    // A provider we are deliberately leaving alone is waiting, not broken. Say
    // which, or the card looks stuck.
    const waitMs = slot(provider.id).backoffUntil - Date.now();
    if (waitMs > 0) {
      note.append(document.createTextNode(` Retrying in ${duration(Math.ceil(waitMs / 1000))}.`));
    }
    section.append(note);
    return section;
  }

  for (const window_ of quota.windows) {
    section.append(renderWindow(window_));
  }

  if (quota.breakdown?.length) {
    const list = el("ul", "slices");
    for (const slice of quota.breakdown) {
      const item = el("li", null, slice.label);
      item.append(el("span", null, `${Math.round(slice.percent)}%`));
      list.append(item);
    }
    section.append(list);
  }

  if (quota.stale) {
    const when = quota.stale.observed_at ? ago(quota.stale.observed_at) : "unknown age";
    const note = el("p", "note stale", `From ${quota.stale.source} · ${when}`);
    if (quota.stale.reason) {
      note.append(document.createElement("br"), document.createTextNode(quota.stale.reason));
      const waitMs = slot(provider.id).backoffUntil - Date.now();
      if (waitMs > 0) {
        note.append(
          document.createTextNode(` Retrying in ${duration(Math.ceil(waitMs / 1000))}.`),
        );
      }
    }
    section.append(note);
  }

  return section;
}

function renderEmptyState(checked) {
  const section = el("section", "provider-empty");
  section.append(el("h2", null, checked ? "No providers detected" : "Checking for providers…"));
  section.append(
    el(
      "p",
      null,
      checked
        ? "Run Claude Code or Codex once, or set up the optional Browser Bridge for Gemini and Grok."
        : "Looking for existing Claude Code, Codex, Gemini, and Grok sign-ins.",
    ),
  );
  return section;
}

function renderProviderControls(missingProviders, checked) {
  providerControlsEl.replaceChildren();
  if (!checked || missingProviders.length === 0) return;

  const button = el(
    "button",
    "manage-providers",
    configuredProviders().length === 0 ? "Set up providers" : "+ Add providers",
  );
  button.type = "button";
  button.setAttribute("aria-expanded", setupExpanded.toString());
  button.addEventListener("click", () => {
    setupExpanded = !setupExpanded;
    render();
  });
  providerControlsEl.append(button);

  if (!setupExpanded) return;

  const panel = el("div", "setup-panel");
  panel.append(
    el(
      "p",
      "setup-intro",
      "Claude and Codex use existing desktop sign-ins. Gemini and Grok use the optional Browser Bridge.",
    ),
  );

  const list = el("ul", "setup-list");
  for (const provider of missingProviders) {
    const item = el("li", "setup-item");
    const head = el("div", "setup-name", provider.name);
    if (provider.optional) head.append(el("span", "setup-optional", "Optional"));
    item.append(head, el("p", null, provider.setup));
    list.append(item);
  }
  panel.append(list);
  if (bridgePath && missingProviders.some(({ optional }) => optional)) {
    panel.append(renderBridgeHelp());
  }
  providerControlsEl.append(panel);
}

// The one thing the setup steps cannot be written down without: where the app
// actually staged the bridge. The MSI and NSIS bundles install to different
// roots, so this has to be asked for rather than assumed.
function renderBridgeHelp() {
  const block = el("div", "bridge-help");
  block.append(
    el(
      "p",
      "bridge-step",
      "Browser Bridge: open chrome://extensions, turn on Developer mode, " +
        "click Load unpacked and pick this folder, then click the extension icon once.",
    ),
    el("code", "bridge-path", bridgePath),
  );

  const copy = el("button", "bridge-action", "Copy path");
  copy.type = "button";
  copy.addEventListener("click", async () => {
    let label = "Copied";
    try {
      await navigator.clipboard.writeText(bridgePath);
    } catch {
      label = "Copy failed";
    }
    copy.textContent = label;
    setTimeout(() => (copy.textContent = "Copy path"), 1500);
  });

  const open = el("button", "bridge-action", "Open folder");
  open.type = "button";
  open.addEventListener("click", async () => {
    try {
      await invoke("reveal_bridge_dir");
    } catch {
      open.textContent = "Could not open";
      setTimeout(() => (open.textContent = "Open folder"), 1500);
    }
  });

  const actions = el("div", "bridge-actions");
  actions.append(copy, open);
  block.append(actions);
  return block;
}

function allProvidersChecked() {
  return PROVIDERS.every(({ id }) => results[id]);
}

function configuredProviders() {
  return PROVIDERS.filter(({ id }) => results[id] && results[id].status !== "not_configured");
}

function render() {
  const checked = allProvidersChecked();
  const configured = configuredProviders();
  const missing = checked
    ? PROVIDERS.filter(({ id }) => results[id].status === "not_configured")
    : [];

  providersEl.replaceChildren(
    ...(configured.length
      ? configured.map((provider) => renderProvider(provider, results[provider.id]))
      : [renderEmptyState(checked)]),
  );
  renderProviderControls(missing, checked);
  publishWidgetSnapshot();
}

function publishWidgetSnapshot() {
  const backoffUntil = Object.fromEntries(
    PROVIDERS.map(({ id }) => [id, schedule[id]?.backoffUntil ?? 0]),
  );
  void emitTo("widget", "quota-snapshot", {
    results,
    backoffUntil,
    updatedAt: lastUpdatedAt?.getTime() ?? null,
    theme: document.documentElement.dataset.theme,
  }).catch(() => {});
}

// ── Polling ─────────────────────────────────────────────────────────────────

const results = {};
const schedule = {};
let inFlight = false;
let lastUpdatedAt = null;
let nextRefreshAt = null;
let refreshTimer = null;
let setupExpanded = false;
let bridgePath = null;

function slot(id) {
  return (schedule[id] ??= { lastAttempt: 0, backoffUntil: 0, recheckAfter: 0, failures: 0 });
}

async function fetchProvider(provider, { userAway = false } = {}) {
  const state = slot(provider.id);
  const now = Date.now();

  if (provider.pauseWhileAway && userAway) return false;

  // A provider that asked us to slow down stays untouched until its backoff ends.
  if (now < state.backoffUntil) return false;
  if (now < state.recheckAfter) return false;
  const providerFloor = Math.max(MIN_GAP_MS, provider.pollMs ?? 0);
  if (now - state.lastAttempt < providerFloor) return false;
  state.lastAttempt = now;

  try {
    results[provider.id] = await invoke(provider.command);
  } catch (error) {
    // A command that throws is a bug in the deck, not a provider outage; say so
    // rather than blaming the vendor.
    results[provider.id] = {
      status: "error",
      message: `The deck could not run its ${provider.name} check: ${error}`,
    };
  }

  const status = results[provider.id]?.status;
  state.backoffUntil = 0;
  state.recheckAfter = 0;

  const retryDelay = providerRetryDelay(
    results[provider.id],
    provider,
    state.failures,
    BACKOFF_MS,
  );

  if (retryDelay !== null) {
    state.backoffUntil = Date.now() + retryDelay;
    state.failures += 1;
  } else {
    // An unavailable quota is a settled answer, not a failure: no escalation,
    // but no point asking again soon either.
    if (status === "unavailable") state.recheckAfter = Date.now() + UNAVAILABLE_RECHECK_MS;
    state.failures = 0;
  }
  return true;
}

function updateRefreshStatus() {
  const checked = allProvidersChecked();
  const hasConfiguredProvider = configuredProviders().length > 0;

  if (checked && !hasConfiguredProvider) {
    updatedEl.textContent = "Waiting for a provider";
    if (inFlight) {
      countdownEl.textContent = "Checking providers…";
      return;
    }
    if (!nextRefreshAt) {
      countdownEl.textContent = "Provider check scheduled";
      return;
    }
    const seconds = Math.max(0, Math.ceil((nextRefreshAt - Date.now()) / 1000));
    countdownEl.textContent = `Checking again in ${duration(seconds)}`;
    return;
  }

  updatedEl.textContent = lastUpdatedAt
    ? `Last updated: ${updateAge(lastUpdatedAt)}`
    : "Last updated: —";

  if (inFlight) {
    countdownEl.textContent = "Refreshing…";
    return;
  }
  if (!nextRefreshAt) {
    countdownEl.textContent = "Refresh scheduled";
    return;
  }
  const seconds = Math.max(0, Math.ceil((nextRefreshAt - Date.now()) / 1000));
  countdownEl.textContent = `Refresh in ${duration(seconds)}`;
}

async function refresh(providers = PROVIDERS, { markUpdated = true } = {}) {
  if (inFlight) return;
  inFlight = true;
  updateRefreshStatus();

  const checksActivity = providers.some((provider) => provider.pauseWhileAway);
  const activity = checksActivity ? await invoke("system_activity").catch(() => null) : null;
  const userAway = isUserAway(activity, IDLE_PAUSE_SECONDS);
  const attempted = await Promise.all(
    providers.map((provider) => fetchProvider(provider, { userAway })),
  );

  render();
  const updatedConfiguredProvider = providers.some(
    (provider, index) => attempted[index] && results[provider.id]?.status !== "not_configured",
  );
  if (markUpdated && updatedConfiguredProvider) lastUpdatedAt = new Date();
  inFlight = false;
  updateRefreshStatus();
}

function scheduleNextRefresh() {
  clearTimeout(refreshTimer);
  nextRefreshAt = Date.now() + POLL_MS;
  const scheduledAt = nextRefreshAt;
  updateRefreshStatus();
  refreshTimer = setTimeout(async () => {
    if (missedRefreshCycle(Date.now(), scheduledAt, POLL_MS)) deferClaudeAfterResume();
    await refresh();
    scheduleNextRefresh();
  }, POLL_MS);
}

await listen("widget-ready", publishWidgetSnapshot);
await listen("widget-preferences-changed", ({ payload }) => applyWidgetPreferences(payload));
applyWidgetPreferences(await invoke("widget_preferences").catch(() => widgetPreferences));

render();
// Resolved once: the staging path only changes if the user moves their profile.
// A failure here just hides the copy/open shortcuts; the README still has the path.
bridgePath = await invoke("bridge_dir").catch(() => null);

// Rust emits this tick outside the WebView. It recovers an overdue global cycle
// when Chromium throttles the hidden dashboard and still lets Claude retry an
// expired 429 independently between those cycles.
function refreshFromNativeTick() {
  if (inFlight) return;
  if (nextRefreshAt && Date.now() >= nextRefreshAt) {
    void (async () => {
      await refresh();
      scheduleNextRefresh();
    })();
    return;
  }
  const claude = PROVIDERS.find(({ id }) => id === "claude");
  if (claude) void refresh([claude], { markUpdated: false });
}

function deferClaudeAfterResume() {
  const state = slot("claude");
  state.recheckAfter = resumeGraceDeadline(
    Date.now(),
    state.recheckAfter,
    CLAUDE_RESUME_GRACE_MS,
  );
}

await listen("system-activity-resumed", deferClaudeAfterResume);
await listen("active-refresh-tick", refreshFromNativeTick);

await refresh();
scheduleNextRefresh();
setInterval(updateRefreshStatus, 1000);

// Countdowns and backoff timers are relative, so they go stale between polls
// even when the data does not.
setInterval(() => {
  if (Object.keys(results).length) render();
}, 30_000);

// Chromium may throttle this WebView while the tray window is hidden. On
// reveal, refresh browser-backed caches and let Claude catch up after an idle or
// locked stretch. Provider floors and backoff still suppress duplicate calls.
const RESUME_PROVIDERS = PROVIDERS.filter(
  ({ id }) => id === "claude" || id === "gemini" || id === "grok",
);

function refreshAfterReveal() {
  if (document.hidden) return;
  if (missedRefreshCycle(Date.now(), nextRefreshAt, POLL_MS)) deferClaudeAfterResume();
  void refresh(RESUME_PROVIDERS, { markUpdated: false });
}

document.addEventListener("visibilitychange", refreshAfterReveal);
window.addEventListener("focus", refreshAfterReveal);
