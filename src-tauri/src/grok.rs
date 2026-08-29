//! Grok quota, read from the OAuth token Grok Build leaves on disk.
//! See ARCHITECTURE.md §5.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::native_host;
use crate::quota::{
    http_failure, now, rfc3339_to_unix, ProviderQuota, QuotaWindow, Stale, UsageSlice,
};

const PROVIDER: &str = "grok";
const DEFAULT_BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";
const BROWSER_CACHE_MAX_AGE_SECONDS: i64 = 5 * 60;
const BROWSER_CACHE_STALE_MAX_AGE_SECONDS: i64 = 24 * 60 * 60;
const BROWSER_CLOCK_SKEW_SECONDS: i64 = 60;

/// ⚠️ `format` selects a **different quota**, not a different rendering of the
/// same one. `credits` is the subscription window a SuperGrok user cares about;
/// `rate_limits` — which is also what you get with no parameter at all — is the
/// xAI API console credit balance, a separate product. On the development
/// account the two read 44% and 13.7% at the same moment, and both look
/// perfectly reasonable in isolation.
const USAGE_PATH: &str = "/billing?format=credits";

/// The dashboard waits on every provider before it repaints, so a request that
/// never answers would freeze the whole refresh cycle, not just this card.
const REQUEST_TIMEOUT_SECONDS: u64 = 10;

fn usage_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECONDS))
        .build()
        .map_err(|e| format!("could not build HTTP client: {e}"))
}

fn base_url() -> String {
    std::env::var("GROK_CLI_CHAT_PROXY_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

fn auth_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".grok").join("auth.json"))
}

/// `auth.json` is a map keyed by `"<oidc_issuer>::<client_id>"` rather than a
/// fixed field name, so the entry has to be found by value, not by path.
#[derive(Deserialize)]
struct AuthEntry {
    key: String,
    #[serde(default)]
    expires_at: Option<String>,
}

#[derive(Deserialize)]
struct BillingResponse {
    config: BillingConfig,
}

#[derive(Deserialize)]
struct BillingConfig {
    #[serde(rename = "currentPeriod", default)]
    current_period: Option<Period>,
    #[serde(rename = "creditUsagePercent", default)]
    credit_usage_percent: Option<f64>,
    #[serde(rename = "productUsage", default)]
    product_usage: Vec<ProductUsage>,
}

#[derive(Deserialize)]
struct Period {
    /// e.g. `USAGE_PERIOD_TYPE_WEEKLY`. The provider naming its own window,
    /// which is what the label must come from.
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    end: Option<String>,
}

#[derive(Deserialize)]
struct ProductUsage {
    #[serde(default)]
    product: Option<String>,
    #[serde(rename = "usagePercent", default)]
    usage_percent: Option<f64>,
}

#[derive(Deserialize)]
struct BrowserCache {
    version: u32,
    provider: String,
    observed_at: i64,
    payload: BrowserPayload,
}

#[derive(Deserialize)]
struct BrowserPayload {
    #[serde(default)]
    unauthorized: bool,
    #[serde(default)]
    paid: Option<BrowserPaid>,
    #[serde(default)]
    buckets: Vec<BrowserBucket>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrowserPaid {
    used: f64,
    #[serde(default)]
    reset_at: Option<f64>,
    #[serde(default)]
    products: Vec<BrowserProduct>,
}

#[derive(Deserialize)]
struct BrowserProduct {
    label: String,
    percent: f64,
}

#[derive(Deserialize)]
struct BrowserBucket {
    label: String,
    used: f64,
    #[serde(default)]
    remaining: Option<i64>,
    #[serde(default)]
    total: Option<i64>,
}

/// `USAGE_PERIOD_TYPE_WEEKLY` → `Weekly`. An unfamiliar period keeps its own
/// shape rather than being forced into a known bucket.
fn period_label(raw: &str) -> String {
    let name = raw.strip_prefix("USAGE_PERIOD_TYPE_").unwrap_or(raw);
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => "Usage".to_string(),
    }
}

/// Pick the credential most likely to be the active session.
///
/// Signing in to a second account adds a second entry, and map iteration order
/// is not stable — taking whichever came first would make the card flip between
/// accounts between polls. An entry that is already expired can never serve a
/// request, so any still-usable one outranks it; a missing `expires_at` means
/// "not known to be expired", not "oldest". Among usable entries the furthest
/// expiry is the freshest sign-in, and the map key breaks ties so the choice is
/// stable between polls.
fn newest_credential(entries: HashMap<String, AuthEntry>, current_time: i64) -> Option<AuthEntry> {
    entries
        .into_iter()
        .max_by_key(|(key, entry)| {
            let expires_at = entry.expires_at.as_deref().and_then(rfc3339_to_unix);
            let usable = expires_at.is_none_or(|at| at > current_time);
            (usable, expires_at, key.clone())
        })
        .map(|(_, entry)| entry)
}

fn paid_credential_quota(result: Result<ProviderQuota, String>) -> ProviderQuota {
    match result {
        Ok(quota) => quota,
        Err(message) => ProviderQuota::error(PROVIDER, message),
    }
}

pub async fn fetch() -> ProviderQuota {
    if let Some(quota) = browser_quota(BROWSER_CACHE_MAX_AGE_SECONDS, None) {
        // Grok Build is sold only to paid accounts, so a credential on disk is
        // itself evidence of one. Fresh browser data reporting free-tier counts
        // contradicts that, which means a signed-out tab pushed the anonymous
        // allowance — ask the credential rather than believe the tab.
        if !(browser_cache_is_free_only() && credential_exists()) {
            return quota;
        }
        // A Grok Build credential proves this is a paid account. Its status
        // must win even when it needs attention; showing anonymous browser
        // counts as "Free" would be a confident account mix-up.
        return paid_credential_quota(try_fetch().await);
    }

    let browser_configured = native_host::cache_exists(PROVIDER).unwrap_or(false);
    let auth_missing = auth_path().is_some_and(|path| matches!(path.try_exists(), Ok(false)));
    let cli_quota = if auth_missing {
        ProviderQuota::not_configured(
            PROVIDER,
            "Install the optional Browser Bridge or sign in with Grok Build to add Grok.",
        )
    } else {
        match try_fetch().await {
            Ok(quota) => quota,
            Err(message) => ProviderQuota::error(PROVIDER, message),
        }
    };
    if matches!(cli_quota, ProviderQuota::Ok { .. }) {
        return cli_quota;
    }

    let reason = match &cli_quota {
        ProviderQuota::Unavailable { message, .. }
        | ProviderQuota::NotConfigured { message, .. }
        | ProviderQuota::ActionRequired { message, .. }
        | ProviderQuota::Error { message, .. } => message.clone(),
        ProviderQuota::Ok { .. } => unreachable!(),
    };
    if let Some(quota) = browser_quota(BROWSER_CACHE_STALE_MAX_AGE_SECONDS, Some(reason)) {
        return quota;
    }
    if browser_configured && matches!(cli_quota, ProviderQuota::NotConfigured { .. }) {
        return ProviderQuota::action_required(
            PROVIDER,
            "Open Grok in a browser so the optional Browser Bridge can update this card.",
        );
    }
    cli_quota
}

async fn try_fetch() -> Result<ProviderQuota, String> {
    let path = auth_path().ok_or("could not locate the home directory")?;
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "cannot read {} — is Grok Build installed and signed in? ({e})",
            path.display()
        )
    })?;
    let entries: HashMap<String, AuthEntry> =
        serde_json::from_str(&raw).map_err(|e| format!("unexpected shape in auth.json: {e}"))?;
    let credential = newest_credential(entries, now()).ok_or("auth.json held no credentials")?;

    if credential
        .expires_at
        .as_deref()
        .and_then(rfc3339_to_unix)
        .is_some_and(|expires_at| expires_at <= now())
    {
        return Ok(expired_token());
    }

    let response = usage_client()?
        .get(format!("{}{}", base_url(), USAGE_PATH))
        .bearer_auth(&credential.key)
        .send()
        .await
        .map_err(|e| format!("request to the billing endpoint failed: {e}"))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        // Grok's token lives about six hours — far shorter than Claude's or
        // Codex's — so this is an action state, not a transient error. The CLI
        // is not a daemon, and repeating this request cannot refresh its token.
        return Ok(expired_token());
    }
    if !response.status().is_success() {
        return Err(http_failure(response.status(), "the Grok billing endpoint"));
    }

    let body: BillingResponse = response
        .json()
        .await
        .map_err(|e| format!("could not parse the billing response: {e}"))?;

    let Some(percent) = body.config.credit_usage_percent else {
        // Reported by the account owner, not measured here: free Grok has no
        // subscription window, only a daily query allowance that this endpoint
        // does not cover.
        return Ok(ProviderQuota::unavailable(
            PROVIDER,
            "No subscription quota on this account. Free Grok is metered by daily queries, \
             which this endpoint does not report.",
        ));
    };

    let period = body.config.current_period.as_ref();
    let starts_at = period
        .and_then(|p| p.start.as_deref())
        .and_then(rfc3339_to_unix);
    let ends_at = period
        .and_then(|p| p.end.as_deref())
        .and_then(rfc3339_to_unix);
    let window = QuotaWindow {
        label: period
            .and_then(|p| p.kind.as_deref())
            .map(period_label)
            .unwrap_or_else(|| "Usage".to_string()),
        percent,
        resets_at: ends_at,
        // Grok gives both ends of the period, so the duration is exact rather
        // than assumed.
        window_seconds: match (starts_at, ends_at) {
            (Some(start), Some(end)) if end > start => Some(end - start),
            _ => None,
        },
        // Grok reports no severity of its own.
        severity: None,
    };

    let breakdown = body
        .config
        .product_usage
        .iter()
        .filter_map(|item| {
            Some(UsageSlice {
                label: item.product.clone()?,
                percent: item.usage_percent?,
            })
        })
        .collect();

    Ok(
        ProviderQuota::ok(PROVIDER, Some("SuperGrok".to_string()), vec![window])
            .with_breakdown(breakdown),
    )
}

fn credential_exists() -> bool {
    auth_path().is_some_and(|path| matches!(path.try_exists(), Ok(true)))
}

fn browser_cache_is_free_only() -> bool {
    native_host::read_cache(PROVIDER)
        .ok()
        .and_then(|raw| serde_json::from_str::<BrowserCache>(&raw).ok())
        .is_some_and(|cache| cache.payload.paid.is_none() && !cache.payload.buckets.is_empty())
}

fn browser_quota(max_age: i64, stale_reason: Option<String>) -> Option<ProviderQuota> {
    let raw = native_host::read_cache(PROVIDER).ok()?;
    browser_quota_from_cache(&raw, now(), max_age, stale_reason)
}

fn browser_quota_from_cache(
    raw: &str,
    current_time: i64,
    max_age: i64,
    stale_reason: Option<String>,
) -> Option<ProviderQuota> {
    let cache: BrowserCache = serde_json::from_str(raw).ok()?;
    if cache.version != 1 || cache.provider != PROVIDER || cache.payload.unauthorized {
        return None;
    }

    let age = current_time - cache.observed_at;
    if !(-BROWSER_CLOCK_SKEW_SECONDS..=max_age).contains(&age) {
        return None;
    }

    let stale = stale_reason.map(|reason| Stale {
        source: "Grok browser extension".to_string(),
        observed_at: Some(cache.observed_at),
        reason,
    });

    if let Some(paid) = cache.payload.paid {
        if !(0.0..=100.0).contains(&paid.used) {
            return None;
        }
        let resets_at = paid
            .reset_at
            .filter(|value| value.is_finite() && *value > 0.0)
            .map(|milliseconds| (milliseconds / 1000.0) as i64);
        if resets_at.is_some_and(|reset| reset <= current_time) {
            return None;
        }
        let breakdown = paid
            .products
            .into_iter()
            .filter(|product| (0.0..=100.0).contains(&product.percent))
            .map(|product| UsageSlice {
                label: product.label,
                percent: product.percent,
            })
            .collect();
        let window = QuotaWindow {
            label: "Weekly".to_string(),
            percent: paid.used,
            resets_at,
            window_seconds: Some(7 * 24 * 60 * 60),
            severity: None,
        };
        let quota = match stale {
            Some(stale) => ProviderQuota::ok_stale(
                PROVIDER,
                Some("SuperGrok".to_string()),
                vec![window],
                stale,
            ),
            None => ProviderQuota::ok(PROVIDER, Some("SuperGrok".to_string()), vec![window]),
        };
        return Some(quota.with_breakdown(breakdown));
    }

    let windows: Vec<QuotaWindow> = cache
        .payload
        .buckets
        .into_iter()
        .filter(|bucket| (0.0..=100.0).contains(&bucket.used))
        .map(|bucket| {
            let label = match (bucket.remaining, bucket.total) {
                (Some(remaining), Some(total)) => {
                    format!("{} · {remaining} / {total} left", bucket.label)
                }
                _ => bucket.label,
            };
            QuotaWindow {
                label,
                percent: bucket.used,
                resets_at: None,
                window_seconds: None,
                severity: None,
            }
        })
        .collect();

    if windows.is_empty() {
        None
    } else {
        Some(match stale {
            Some(stale) => {
                ProviderQuota::ok_stale(PROVIDER, Some("Free".to_string()), windows, stale)
            }
            None => ProviderQuota::ok(PROVIDER, Some("Free".to_string()), windows),
        })
    }
}

fn expired_token() -> ProviderQuota {
    ProviderQuota::action_required(
        PROVIDER,
        "Open Grok Build once so it can refresh its sign-in, then refresh this card.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_period_from_the_providers_own_enum() {
        assert_eq!(period_label("USAGE_PERIOD_TYPE_WEEKLY"), "Weekly");
        assert_eq!(period_label("USAGE_PERIOD_TYPE_MONTHLY"), "Monthly");
        assert_eq!(period_label("USAGE_PERIOD_TYPE_DAILY"), "Daily");
    }

    #[test]
    fn passes_an_unfamiliar_period_through() {
        // A period xAI adds later should look odd, not be mislabelled.
        assert_eq!(period_label("USAGE_PERIOD_TYPE_FORTNIGHTLY"), "Fortnightly");
        assert_eq!(period_label("SOMETHING_ELSE"), "Something_else");
    }

    #[test]
    fn paid_credential_attention_is_never_relabelled_as_free() {
        assert!(matches!(
            paid_credential_quota(Ok(expired_token())),
            ProviderQuota::ActionRequired {
                provider: "grok",
                ..
            }
        ));
        assert!(matches!(
            paid_credential_quota(Err("offline".to_string())),
            ProviderQuota::Error {
                provider: "grok",
                ..
            }
        ));
    }

    #[test]
    fn picks_the_credential_with_the_furthest_expiry() {
        let json = r#"{
            "https://auth.x.ai::old": { "key": "stale-token",
                                        "expires_at": "2026-01-01T00:00:00Z" },
            "https://auth.x.ai::new": { "key": "fresh-token",
                                        "expires_at": "2026-06-01T00:00:00Z" }
        }"#;
        let entries: HashMap<String, AuthEntry> = serde_json::from_str(json).unwrap();
        let before_both = rfc3339_to_unix("2025-12-01T00:00:00Z").unwrap();
        assert_eq!(
            newest_credential(entries, before_both).unwrap().key,
            "fresh-token"
        );
    }

    #[test]
    fn survives_a_credential_with_no_expiry() {
        let json = r#"{ "only": { "key": "the-token" } }"#;
        let entries: HashMap<String, AuthEntry> = serde_json::from_str(json).unwrap();
        assert_eq!(newest_credential(entries, 0).unwrap().key, "the-token");
    }

    #[test]
    fn a_usable_credential_without_expiry_beats_an_expired_one_with_a_timestamp() {
        // `None < Some(_)` must not decide this: the timestamped entry is dead,
        // the undated one is the only credential that can still work.
        let json = r#"{
            "https://auth.x.ai::expired": { "key": "dead-token",
                                            "expires_at": "2026-01-01T00:00:00Z" },
            "https://auth.x.ai::open":    { "key": "live-token" }
        }"#;
        let entries: HashMap<String, AuthEntry> = serde_json::from_str(json).unwrap();
        let after_expiry = rfc3339_to_unix("2026-03-01T00:00:00Z").unwrap();
        assert_eq!(
            newest_credential(entries, after_expiry).unwrap().key,
            "live-token"
        );
    }

    #[test]
    fn a_future_expiry_beats_an_expired_one() {
        let json = r#"{
            "https://auth.x.ai::expired": { "key": "dead-token",
                                            "expires_at": "2026-01-01T00:00:00Z" },
            "https://auth.x.ai::live":    { "key": "live-token",
                                            "expires_at": "2026-06-01T00:00:00Z" }
        }"#;
        let entries: HashMap<String, AuthEntry> = serde_json::from_str(json).unwrap();
        let between = rfc3339_to_unix("2026-03-01T00:00:00Z").unwrap();
        assert_eq!(
            newest_credential(entries, between).unwrap().key,
            "live-token"
        );
    }

    #[test]
    fn an_expired_cli_token_requires_action_instead_of_retry_backoff() {
        let ProviderQuota::ActionRequired { message, .. } = expired_token() else {
            panic!("expired Grok credentials must not be a retryable error");
        };
        assert!(message.contains("Open Grok Build once"));
    }

    #[test]
    fn fresh_paid_browser_data_wins_with_its_breakdown() {
        let raw = r#"{
            "version": 1,
            "provider": "grok",
            "observed_at": 1000,
            "payload": {
                "buckets": [],
                "unauthorized": false,
                "paid": {
                    "used": 42.5,
                    "resetAt": 1800000000000,
                    "products": [
                        { "label": "Chat", "percent": 30.0 },
                        { "label": "Imagine", "percent": 12.5 }
                    ]
                }
            }
        }"#;

        let ProviderQuota::Ok {
            windows, breakdown, ..
        } = browser_quota_from_cache(raw, 1200, BROWSER_CACHE_MAX_AGE_SECONDS, None)
            .expect("fresh browser quota")
        else {
            panic!("fresh browser data must produce a quota card");
        };
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "Weekly");
        assert_eq!(windows[0].percent, 42.5);
        assert_eq!(breakdown.len(), 2);
    }

    #[test]
    fn stale_browser_data_falls_through_to_the_cli() {
        let raw = r#"{
            "version": 1,
            "provider": "grok",
            "observed_at": 1000,
            "payload": {
                "unauthorized": false,
                "paid": { "used": 42.5, "resetAt": null, "products": [] },
                "buckets": []
            }
        }"#;
        assert!(browser_quota_from_cache(
            raw,
            1000 + BROWSER_CACHE_MAX_AGE_SECONDS + 1,
            BROWSER_CACHE_MAX_AGE_SECONDS,
            None,
        )
        .is_none());
    }

    #[test]
    fn free_browser_data_keeps_query_counts_visible() {
        let raw = r#"{
            "version": 1,
            "provider": "grok",
            "observed_at": 1000,
            "payload": {
                "unauthorized": false,
                "paid": null,
                "buckets": [
                    { "label": "Fast", "used": 25, "remaining": 30, "total": 40 }
                ]
            }
        }"#;

        let ProviderQuota::Ok { plan, windows, .. } =
            browser_quota_from_cache(raw, 1100, BROWSER_CACHE_MAX_AGE_SECONDS, None)
                .expect("fresh free-tier quota")
        else {
            panic!("free browser data must produce a quota card");
        };
        assert_eq!(plan.as_deref(), Some("Free"));
        assert_eq!(windows[0].label, "Fast · 30 / 40 left");
        assert_eq!(windows[0].percent, 25.0);
    }

    #[test]
    fn stale_browser_data_is_labelled_when_the_cli_cannot_help() {
        let raw = r#"{
            "version": 1,
            "provider": "grok",
            "observed_at": 1000,
            "payload": {
                "unauthorized": false,
                "paid": null,
                "buckets": [
                    { "label": "Fast", "used": 25, "remaining": 30, "total": 40 }
                ]
            }
        }"#;

        let ProviderQuota::Ok { stale, .. } = browser_quota_from_cache(
            raw,
            1000 + BROWSER_CACHE_MAX_AGE_SECONDS + 1,
            BROWSER_CACHE_STALE_MAX_AGE_SECONDS,
            Some("CLI token expired".to_string()),
        )
        .expect("stale browser fallback") else {
            panic!("stale browser data should remain a quota card");
        };
        let stale = stale.expect("fallback must show its age");
        assert_eq!(stale.observed_at, Some(1000));
        assert_eq!(stale.reason, "CLI token expired");
    }

    #[test]
    fn paid_browser_data_never_survives_its_reported_reset() {
        let raw = r#"{
            "version": 1,
            "provider": "grok",
            "observed_at": 1000,
            "payload": {
                "unauthorized": false,
                "paid": { "used": 42.5, "resetAt": 1200000, "products": [] },
                "buckets": []
            }
        }"#;

        assert!(browser_quota_from_cache(
            raw,
            1201,
            BROWSER_CACHE_STALE_MAX_AGE_SECONDS,
            Some("CLI token expired".to_string()),
        )
        .is_none());
    }
}
