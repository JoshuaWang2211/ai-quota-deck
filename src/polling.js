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

export function providerRetryDelay(result, provider, failures, defaultBackoffMs) {
  const retryAfterSeconds = result?.retry_after_seconds;
  const rateLimited =
    typeof retryAfterSeconds === "number" && Number.isFinite(retryAfterSeconds);

  if (result?.status !== "error" && !rateLimited) return null;

  // The backend owns the 429 cooldown — for Claude it escalates and persists it
  // in claude-rate-limit.json — and reports how long is left. No second table
  // here; the provider's own poll floor just keeps the retry no more eager
  // than a healthy poll.
  if (rateLimited) {
    return Math.max(Math.max(retryAfterSeconds, 0) * 1000, provider.pollMs ?? 0);
  }
  return Math.max(
    defaultBackoffMs[Math.min(failures, defaultBackoffMs.length - 1)],
    provider.pollMs ?? 0,
  );
}
