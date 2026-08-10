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

  const backoff = rateLimited ? provider.rateLimitBackoffMs ?? defaultBackoffMs : defaultBackoffMs;
  let delay = Math.max(
    backoff[Math.min(failures, backoff.length - 1)],
    rateLimited ? Math.max(retryAfterSeconds, 0) * 1000 : 0,
    provider.pollMs ?? 0,
  );
  if (rateLimited && provider.maxRateLimitBackoffMs) {
    delay = Math.min(delay, provider.maxRateLimitBackoffMs);
  }
  return delay;
}
