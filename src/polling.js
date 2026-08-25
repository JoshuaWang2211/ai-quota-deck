export function isUserAway(activity, idlePauseSeconds) {
  return Boolean(
    activity?.workstation_locked || activity?.idle_seconds >= idlePauseSeconds,
  );
}

export function resumeGraceDeadline(now, currentDeadline, graceMs) {
  return Math.max(currentDeadline ?? 0, now + graceMs);
}

export function missedRefreshCycle(now, scheduledAt, cycleMs) {
  return Boolean(scheduledAt && now - scheduledAt >= cycleMs);
}

// A provider the user unticked leaves the schedule entirely: no card, no
// request. Ids the hide-list names but this build does not know are ignored.
export function enabledProviders(providers, hiddenProviders = []) {
  return providers.filter(({ id }) => !hiddenProviders.includes(id));
}

// A backend-provided 429 deadline replaces the healthy polling cadence for
// that retry. Rust has already guaranteed that the deadline is no earlier than
// Claude's healthy floor. The small global gap still protects duplicate window
// lifecycle events firing the same command together.

export function providerRequestFloor(provider, retryingRateLimit, minGapMs) {
  return retryingRateLimit
    ? minGapMs
    : Math.max(minGapMs, provider.pollMs ?? 0);
}

export function providerRetryDelay(result, provider, failures, defaultBackoffMs) {
  const retryAfterSeconds = result?.retry_after_seconds;
  const rateLimited =
    typeof retryAfterSeconds === "number" && Number.isFinite(retryAfterSeconds);

  if (result?.status !== "error" && !rateLimited) return null;

  // The backend owns the 429 cooldown and reports how long is left. Do not
  // lengthen it with the healthy cadence; fetchProvider uses the duplicate-event
  // floor, rather than pollMs, when this retry becomes due.
  if (rateLimited) {
    return Math.max(retryAfterSeconds, 0) * 1000;
  }
  return Math.max(
    defaultBackoffMs[Math.min(failures, defaultBackoffMs.length - 1)],
    provider.pollMs ?? 0,
  );
}
