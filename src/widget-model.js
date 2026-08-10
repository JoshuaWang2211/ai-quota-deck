export const WIDGET_PROVIDERS = [
  { id: "claude", name: "Claude" },
  { id: "codex", name: "Codex" },
  { id: "gemini", name: "Gemini" },
  { id: "grok", name: "Grok" },
];

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

export function widgetWindows(providerId, windows = []) {
  return providerId === "grok" ? windows.filter(isSevenDayWindow).slice(0, 1) : windows;
}

export function quotaTone(percent) {
  if (percent >= 90) return "critical";
  if (percent >= 70) return "warning";
  return "ok";
}
