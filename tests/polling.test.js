import assert from "node:assert/strict";
import test from "node:test";

import {
  isUserAway,
  missedRefreshCycle,
  providerRetryDelay,
  resumeGraceDeadline,
} from "../src/polling.js";
import {
  compactWindowLabel,
  quotaTone,
  widgetWindows,
} from "../src/widget-model.js";

const defaults = [60_000, 120_000, 300_000];
const claude = {
  pollMs: 360_000,
  rateLimitBackoffMs: [180_000, 360_000, 720_000, 900_000],
  maxRateLimitBackoffMs: 900_000,
};

test("Claude pauses at five idle minutes and immediately on lock", () => {
  assert.equal(isUserAway({ idle_seconds: 299, workstation_locked: false }, 300), false);
  assert.equal(isUserAway({ idle_seconds: 300, workstation_locked: false }, 300), true);
  assert.equal(isUserAway({ idle_seconds: 0, workstation_locked: true }, 300), true);
  assert.equal(isUserAway(null, 300), false);
});

test("Claude waits after resume without shortening an existing provider delay", () => {
  assert.equal(resumeGraceDeadline(1_000, 0, 60_000), 61_000);
  assert.equal(resumeGraceDeadline(1_000, 90_000, 60_000), 90_000);
});

test("a timer delayed by sleep is treated as a resume instead of an immediate Claude poll", () => {
  assert.equal(missedRefreshCycle(500_000, 180_000, 180_000), true);
  assert.equal(missedRefreshCycle(359_999, 180_000, 180_000), false);
  assert.equal(missedRefreshCycle(500_000, null, 180_000), false);
});

test("cached Claude rows preserve the 429 backoff", () => {
  const cached = { status: "ok", retry_after_seconds: 180 };
  assert.equal(providerRetryDelay(cached, claude, 0, defaults), 360_000);
  assert.equal(providerRetryDelay(cached, claude, 1, defaults), 360_000);
  assert.equal(providerRetryDelay(cached, claude, 2, defaults), 720_000);
  assert.equal(providerRetryDelay(cached, claude, 3, defaults), 900_000);
});

test("Retry-After is honored within the fifteen-minute ceiling", () => {
  assert.equal(
    providerRetryDelay({ status: "ok", retry_after_seconds: 420 }, claude, 0, defaults),
    420_000,
  );
  assert.equal(
    providerRetryDelay({ status: "error", retry_after_seconds: 3600 }, claude, 0, defaults),
    900_000,
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

test("widget colors match the extension thresholds", () => {
  assert.equal(quotaTone(69.9), "ok");
  assert.equal(quotaTone(70), "warning");
  assert.equal(quotaTone(90), "critical");
});
