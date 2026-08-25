import assert from "node:assert/strict";
import test from "node:test";

import {
  enabledProviders,
  isUserAway,
  missedRefreshCycle,
  providerRequestFloor,
  providerRetryDelay,
  resumeGraceDeadline,
} from "../src/polling.js";
import {
  compactProviderName,
  compactWindowLabel,
  quotaTone,
  visibleProviders,
  WIDGET_PROVIDERS,
  widgetWindows,
} from "../src/widget-model.js";

const defaults = [60_000, 120_000, 300_000];
const claude = { pollMs: 360_000 };

test("Claude pauses at five idle minutes and immediately on lock", () => {
  assert.equal(isUserAway({ idle_seconds: 299, workstation_locked: false }, 300), false);
  assert.equal(isUserAway({ idle_seconds: 300, workstation_locked: false }, 300), true);
  assert.equal(isUserAway({ idle_seconds: 0, workstation_locked: true }, 300), true);
  assert.equal(isUserAway(null, 300), false);
});

test("Claude waits after resume without shortening an existing provider delay", () => {
  assert.equal(resumeGraceDeadline(1_000, 0, 120_000), 121_000);
  assert.equal(resumeGraceDeadline(1_000, 150_000, 120_000), 150_000);
});

test("a timer delayed by sleep is treated as a resume instead of an immediate Claude poll", () => {
  assert.equal(missedRefreshCycle(500_000, 180_000, 180_000), true);
  assert.equal(missedRefreshCycle(359_999, 180_000, 180_000), false);
  assert.equal(missedRefreshCycle(500_000, null, 180_000), false);
});

test("cached Claude rows follow the cooldown the backend reports", () => {
  // Rust owns the 6/12/24/48/60-minute fallback and five-second edge buffer;
  // the frontend preserves the exact remaining cooldown it receives.
  const cooldownStart = { status: "ok", retry_after_seconds: 365 };
  assert.equal(providerRetryDelay(cooldownStart, claude, 0, defaults), 365_000);
  assert.equal(providerRetryDelay(cooldownStart, claude, 3, defaults), 365_000);
  assert.equal(
    providerRetryDelay({ status: "ok", retry_after_seconds: 1078 }, claude, 0, defaults),
    1_078_000,
  );
});

test("a due 429 retry bypasses only the healthy provider cadence", () => {
  assert.equal(providerRequestFloor(claude, false, 20_000), 360_000);
  assert.equal(providerRequestFloor(claude, true, 20_000), 20_000);
});

test("backend Retry-After deadlines pass through without a frontend cap", () => {
  assert.equal(
    providerRetryDelay({ status: "ok", retry_after_seconds: 1078 }, claude, 0, defaults),
    1_078_000,
  );
  assert.equal(
    providerRetryDelay({ status: "error", retry_after_seconds: 3605 }, claude, 0, defaults),
    3_605_000,
  );
});

test("ordinary errors retain the existing provider-local retry schedule", () => {
  assert.equal(providerRetryDelay({ status: "error" }, {}, 0, defaults), 60_000);
  assert.equal(providerRetryDelay({ status: "error" }, {}, 2, defaults), 300_000);
  assert.equal(providerRetryDelay({ status: "ok" }, claude, 0, defaults), null);
  assert.equal(
    providerRetryDelay({ status: "ok", retry_after_seconds: null }, claude, 0, defaults),
    null,
  );
});

test("widget labels quota windows by duration rather than slot", () => {
  assert.equal(compactWindowLabel({ label: "Session (5h)", window_seconds: 18_000 }), "5h");
  assert.equal(compactWindowLabel({ label: "Weekly", window_seconds: 604_800 }), "7d");
  assert.equal(compactWindowLabel({ label: "Monthly", window_seconds: 2_592_000 }), "30d");
});

test("widget shows only Grok's seven-day pool", () => {
  const windows = [
    { label: "Daily", window_seconds: 86_400 },
    { label: "Weekly", window_seconds: 604_800 },
  ];
  assert.deepEqual(widgetWindows("grok", windows), [windows[1]]);
  assert.deepEqual(widgetWindows("gemini", windows), windows);
});

test("widget keeps both Antigravity weekly pools and drops the five-hour ones", () => {
  const windows = [
    { label: "Weekly · Gemini", window_seconds: 604_800 },
    { label: "Session (5h) · Gemini", window_seconds: 18_000 },
    { label: "Weekly · Claude+GPT", window_seconds: 604_800 },
    { label: "Session (5h) · Claude+GPT", window_seconds: 18_000 },
  ];
  assert.deepEqual(widgetWindows("antigravity", windows), [windows[0], windows[2]]);
  assert.equal(compactWindowLabel(windows[0]), "Gemini");
  assert.equal(compactWindowLabel(windows[2]), "Claude+GPT");
});

test("strip shortens provider titles and widget keeps the full name", () => {
  assert.deepEqual(
    WIDGET_PROVIDERS.map((provider) => compactProviderName(provider, true)),
    ["CL", "CO", "AG", "GE", "GR"],
  );
  assert.equal(compactProviderName(WIDGET_PROVIDERS[0], false), "Claude");
});

test("widget colors match the extension thresholds", () => {
  assert.equal(quotaTone(69.9), "ok");
  assert.equal(quotaTone(70), "warning");
  assert.equal(quotaTone(90), "critical");
});

test("a hidden provider leaves the schedule and unknown ids are ignored", () => {
  const providers = [{ id: "claude" }, { id: "codex" }];
  assert.deepEqual(enabledProviders(providers, ["claude"]), [providers[1]]);
  assert.deepEqual(enabledProviders(providers, []), providers);
  assert.deepEqual(enabledProviders(providers), providers);
  assert.deepEqual(enabledProviders(providers, ["antigravity"]), providers);
});

test("widget hides unticked and unconfigured providers but keeps broken ones", () => {
  const results = {
    claude: { status: "ok" },
    codex: { status: "not_configured" },
    gemini: { status: "error" },
    grok: { status: "unavailable" },
  };
  const ids = (providers) => providers.map(({ id }) => id);
  assert.deepEqual(ids(visibleProviders(WIDGET_PROVIDERS, results, ["claude"])), [
    "gemini",
    "grok",
  ]);
  assert.deepEqual(ids(visibleProviders(WIDGET_PROVIDERS, results)), [
    "claude",
    "gemini",
    "grok",
  ]);
  assert.deepEqual(ids(visibleProviders(WIDGET_PROVIDERS, results, ["antigravity"])), [
    "claude",
    "gemini",
    "grok",
  ]);
  assert.deepEqual(visibleProviders(WIDGET_PROVIDERS, {}, []), []);
});
