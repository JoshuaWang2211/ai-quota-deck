import {
  compactWindowLabel,
  quotaTone,
  WIDGET_PROVIDERS,
  widgetWindows,
} from "./widget-model.js";

const { invoke } = window.__TAURI__.core;
const { emitTo, listen } = window.__TAURI__.event;

const providersEl = document.getElementById("widget-providers");
const statusEl = document.getElementById("widget-status");
const dragHandleEl = document.getElementById("widget-drag-handle");
const widgetEl = document.querySelector(".widget");
const openDashboardEl = document.getElementById("open-dashboard");
const lockWidgetEl = document.getElementById("lock-widget");
const hideWidgetEl = document.getElementById("hide-widget");

let snapshot = { results: {}, backoffUntil: {}, updatedAt: null };
let preferences = { visible: false, locked: false, strip: false };
let resizeFrame = null;

const nowSec = () => Math.floor(Date.now() / 1000);

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function duration(seconds) {
  const days = Math.floor(seconds / 86_400);
  const hours = Math.floor((seconds % 86_400) / 3_600);
  const minutes = Math.floor((seconds % 3_600) / 60);
  if (days) return `${days}d ${hours}h`;
  if (hours) return `${hours}h ${minutes}m`;
  if (minutes) return `${minutes}m`;
  return `${Math.max(0, seconds)}s`;
}

function age(timestampMs) {
  if (!timestampMs) return "Waiting for data";
  const seconds = Math.max(0, Math.floor((Date.now() - timestampMs) / 1_000));
  if (seconds < 60) return "Updated just now";
  return `Updated ${duration(seconds)} ago`;
}

function staleDetails(providerId, quota) {
  if (!quota?.stale) return null;
  const details = ["Cached data"];
  if (quota.stale.observed_at) {
    details[0] = `Cached · ${duration(Math.max(0, nowSec() - quota.stale.observed_at))} old`;
  }
  if (quota.stale.reason) details.push(quota.stale.reason);
  const waitMs = (snapshot.backoffUntil[providerId] ?? 0) - Date.now();
  if (waitMs > 0) details.push(`Retrying in ${duration(Math.ceil(waitMs / 1_000))}`);
  return details.join(" · ");
}

function renderMetric(window_) {
  const metric = el("span", `widget-metric ${quotaTone(window_.percent)}`);
  const reset = window_.resets_at
    ? window_.resets_at > nowSec()
      ? ` · Resets in ${duration(window_.resets_at - nowSec())}`
      : " · Resetting now"
    : "";
  metric.title = `${window_.label}: ${Math.round(window_.percent)}% used${reset}`;
  metric.append(
    el("span", "metric-period", compactWindowLabel(window_)),
    el("span", "metric-value", `${Math.round(window_.percent)}%`),
  );
  return metric;
}

function renderProvider(provider, quota) {
  const row = el("section", `widget-provider${quota?.stale ? " cached" : ""}`);
  const identity = el("div", "provider-identity");
  const name = el("span", "provider-name", provider.name);
  if (quota?.plan) name.title = quota.plan;
  identity.append(name);

  const stale = staleDetails(provider.id, quota);
  if (stale) {
    const cached = el("span", "cached-label", "cached");
    cached.title = stale;
    identity.append(cached);
    row.title = stale;
  }
  row.append(identity);

  const metrics = el("div", "widget-metrics");
  if (!quota) {
    metrics.append(el("span", "provider-state", "…"));
  } else if (quota.status !== "ok") {
    const quiet = quota.status === "unavailable" || quota.status === "not_configured";
    const state = el("span", `provider-state${quiet ? "" : " problem"}`, quiet ? "—" : "!");
    if (quota.message) state.title = quota.message;
    metrics.append(state);
  } else {
    const windows = widgetWindows(provider.id, quota.windows);
    if (!windows.length) {
      const missing = el("span", "widget-metric unavailable");
      missing.title = `${provider.name} does not report a 7-day quota.`;
      missing.append(el("span", "metric-period", "7d"), el("span", "metric-value", "—"));
      metrics.append(missing);
    } else {
      metrics.append(...windows.map(renderMetric));
    }
  }
  row.append(metrics);
  return row;
}

function scheduleResize() {
  if (resizeFrame !== null) cancelAnimationFrame(resizeFrame);
  resizeFrame = requestAnimationFrame(() => {
    resizeFrame = null;
    const width = preferences.strip ? stripContentWidth() : widgetContentWidth();
    const height = preferences.strip
      ? 40
      : Math.ceil(widgetEl.getBoundingClientRect().height + 12);
    void invoke("resize_widget", { width, height }).catch(() => {});
  });
}

function number(value) {
  return Number.parseFloat(value) || 0;
}

function horizontalExtras(node) {
  const style = getComputedStyle(node);
  return (
    number(style.paddingLeft) +
    number(style.paddingRight) +
    number(style.borderLeftWidth) +
    number(style.borderRightWidth)
  );
}

function visibleChildrenWidth(node) {
  const children = [...node.children].filter(
    (child) => getComputedStyle(child).display !== "none",
  );
  const style = getComputedStyle(node);
  const gap = number(style.columnGap || style.gap);
  return children.reduce((total, child) => {
    const childStyle = getComputedStyle(child);
    return (
      total +
      Math.max(child.scrollWidth, child.getBoundingClientRect().width) +
      number(childStyle.marginLeft) +
      number(childStyle.marginRight)
    );
  }, Math.max(0, children.length - 1) * gap);
}

function metricsWidth(metrics) {
  const metricsStyle = getComputedStyle(metrics);
  const metricsNodes = [...metrics.children];
  const gap = number(metricsStyle.columnGap || metricsStyle.gap);
  return metricsNodes.reduce(
    (total, metric) =>
      total +
      (metric.children.length
        ? visibleChildrenWidth(metric)
        : Math.max(metric.scrollWidth, metric.getBoundingClientRect().width)),
    Math.max(0, metricsNodes.length - 1) * gap,
  );
}

function widgetContentWidth() {
  const headWidth = horizontalExtras(dragHandleEl) + visibleChildrenWidth(dragHandleEl);
  const rowWidths = [...providersEl.querySelectorAll(".widget-provider")].map((row) => {
    const style = getComputedStyle(row);
    const identityColumn = number(style.gridTemplateColumns.split(" ")[0]);
    return (
      horizontalExtras(row) +
      identityColumn +
      number(style.columnGap) +
      metricsWidth(row.querySelector(".widget-metrics"))
    );
  });
  const contentWidth = Math.max(headWidth, ...rowWidths, 188);
  return Math.ceil(contentWidth + horizontalExtras(widgetEl) + horizontalExtras(document.body));
}

function stripContentWidth() {
  const providerWidth = [...providersEl.querySelectorAll(".widget-provider")].reduce(
    (total, row) => {
      const style = getComputedStyle(row);
      return (
        total +
        horizontalExtras(row) +
        visibleChildrenWidth(row.querySelector(".provider-identity")) +
        number(style.columnGap) +
        metricsWidth(row.querySelector(".widget-metrics"))
      );
    },
    0,
  );
  const controlsWidth = horizontalExtras(dragHandleEl) + visibleChildrenWidth(dragHandleEl);
  return Math.ceil(
    providerWidth +
      controlsWidth +
      horizontalExtras(widgetEl) +
      horizontalExtras(document.body),
  );
}

function render() {
  const configured = WIDGET_PROVIDERS.filter(
    ({ id }) => snapshot.results[id] && snapshot.results[id].status !== "not_configured",
  );
  providersEl.replaceChildren(
    ...(configured.length
      ? configured.map((provider) => renderProvider(provider, snapshot.results[provider.id]))
      : [el("p", "widget-empty", "No providers detected")]),
  );
  statusEl.textContent = age(snapshot.updatedAt);
  scheduleResize();
}

function applyPreferences(next) {
  preferences = { ...preferences, ...next };
  document.documentElement.dataset.locked = preferences.locked.toString();
  document.documentElement.dataset.mode = preferences.strip ? "strip" : "widget";
  widgetEl.setAttribute(
    "aria-label",
    preferences.strip ? "AI quota strip" : "AI quota widget",
  );
  lockWidgetEl.title = preferences.locked ? "Unlock widget position" : "Lock widget position";
  lockWidgetEl.setAttribute(
    "aria-label",
    preferences.locked ? "Unlock widget position" : "Lock widget position",
  );
  hideWidgetEl.title = preferences.strip ? "Hide strip" : "Hide widget";
  hideWidgetEl.setAttribute("aria-label", hideWidgetEl.title);
  scheduleResize();
}

widgetEl.addEventListener("pointerdown", (event) => {
  if (
    event.button !== 0 ||
    (!preferences.strip && preferences.locked) ||
    (!preferences.strip && !event.target.closest(".widget-head")) ||
    event.target.closest("button")
  )
    return;
  event.preventDefault();
  void invoke("start_widget_drag").catch(() => {});
});

widgetEl.addEventListener("dblclick", (event) => {
  if (event.target.closest("button")) return;
  if (!preferences.strip && !event.target.closest(".widget-head")) return;
  void invoke("open_dashboard");
});

openDashboardEl.addEventListener("click", () => void invoke("open_dashboard"));
hideWidgetEl.addEventListener("click", () => void invoke("hide_companion"));
lockWidgetEl.addEventListener("click", () => {
  void invoke("set_widget_locked", { locked: !preferences.locked });
});

await listen("quota-snapshot", ({ payload }) => {
  snapshot = payload;
  if (payload.theme) document.documentElement.dataset.theme = payload.theme;
  render();
});
await listen("widget-theme-changed", ({ payload }) => {
  document.documentElement.dataset.theme = payload;
});
await listen("widget-preferences-changed", ({ payload }) => applyPreferences(payload));

applyPreferences(await invoke("widget_preferences").catch(() => preferences));
render();
await emitTo("main", "widget-ready").catch(() => {});
setInterval(() => {
  statusEl.textContent = age(snapshot.updatedAt);
  if (Object.keys(snapshot.results).length) render();
}, 30_000);
