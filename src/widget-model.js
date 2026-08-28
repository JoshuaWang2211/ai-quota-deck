export const WIDGET_PROVIDERS = [
  { id: "claude", name: "Claude", stripName: "CL" },
  { id: "codex", name: "Codex", stripName: "CO" },
  { id: "antigravity", name: "Antigravity", stripName: "AG" },
  { id: "gemini", name: "Gemini", stripName: "GE" },
  { id: "grok", name: "Grok", stripName: "GR" },
  { id: "grok_bot", name: "Grok Bot", stripName: "GB" },
];

export function compactProviderName(provider, strip) {
  return strip ? provider.stripName : provider.name;
}

// The snapshot still carries results for hidden providers (their schedule and
// cached rows survive a hide), so the companion filters by preference itself.
export function visibleProviders(providers, results, hiddenProviders = []) {
  return providers.filter(
    ({ id }) =>
      !hiddenProviders.includes(id) && results[id] && results[id].status !== "not_configured",
  );
}

export function isSevenDayWindow(window_) {
  return window_.window_seconds === 7 * 24 * 60 * 60 || /^weekly\b/i.test(window_.label);
}

export function compactWindowLabel(window_) {
  const scopedLabel = window_.label.split("·")[1]?.trim();
  if (scopedLabel) return scopedLabel;

  const seconds = window_.window_seconds;
  if (seconds === 5 * 60 * 60 || /^session\b/i.test(window_.label)) return "5h";
  if (seconds === 24 * 60 * 60 || /^daily\b/i.test(window_.label)) return "1d";
  if (isSevenDayWindow(window_)) return "7d";
  if (seconds === 30 * 24 * 60 * 60 || /^monthly\b/i.test(window_.label)) return "30d";
  return window_.label.length > 8 ? `${window_.label.slice(0, 7)}…` : window_.label;
}

// Grok shows its single seven-day pool; Grok Bot already reports one weekly
// pool. Antigravity shows the weekly bucket of each of its two pools. The
// five-hour buckets stay on the dashboard.
export function widgetWindows(providerId, windows = []) {
  if (providerId === "grok") return windows.filter(isSevenDayWindow).slice(0, 1);
  if (providerId === "grok_bot") return windows.filter(isSevenDayWindow).slice(0, 1);
  if (providerId === "antigravity") return windows.filter(isSevenDayWindow);
  return windows;
}

export function quotaTone(percent) {
  if (percent >= 90) return "critical";
  if (percent >= 70) return "warning";
  return "ok";
}
