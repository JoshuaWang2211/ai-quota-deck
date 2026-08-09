import assert from "node:assert/strict";
import test from "node:test";

import { isUserAway, providerRetryDelay } from "../src/polling.js";

const defaults = [60_000, 120_000, 300_000];
const claude = {
  pollMs: 120_000,
  rateLimitBackoffMs: [180_000, 360_000, 720_000, 900_000],
  maxRateLimitBackoffMs: 900_000,
};

test("Claude pauses at five idle minutes and immediately on lock", () => {
  assert.equal(isUserAway({ idle_seconds: 299, workstation_locked: false }, 300), false);
  assert.equal(isUserAway({ idle_seconds: 300, workstation_locked: false }, 300), true);
  assert.equal(isUserAway({ idle_seconds: 0, workstation_locked: true }, 300), true);
  assert.equal(isUserAway(null, 300), false);
});

test("cached Claude rows preserve the 429 backoff", () => {
  const cached = { status: "ok", retry_after_seconds: 180 };
  assert.equal(providerRetryDelay(cached, claude, 0, defaults), 180_000);
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
