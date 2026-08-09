//! Gemini quota, pushed by the browser extension through Native Messaging.
//! See ARCHITECTURE.md §6.

use serde::Deserialize;

use crate::native_host;
use crate::quota::{now, ProviderQuota, QuotaWindow, Stale};

const PROVIDER: &str = "gemini";
const FRESH_MAX_AGE_SECONDS: i64 = 5 * 60;
const CACHE_MAX_AGE_SECONDS: i64 = 7 * 24 * 60 * 60;
const CLOCK_SKEW_SECONDS: i64 = 60;

#[derive(Deserialize)]
struct BrowserCache {
    version: u32,
    provider: String,
    observed_at: i64,
    payload: BrowserPayload,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrowserPayload {
    #[serde(rename = "account_id")]
    account_id: String,
    #[serde(default)]
    tier: Option<i64>,
    ratio5h: f64,
    #[serde(default)]
    reset_time5h: Option<i64>,
    ratio7d: f64,
    #[serde(default)]
    reset_time7d: Option<i64>,
}

pub async fn fetch() -> ProviderQuota {
    let raw = match native_host::read_cache(PROVIDER) {
        Ok(raw) => raw,
        Err(_) if matches!(native_host::cache_exists(PROVIDER), Ok(false)) => {
            return ProviderQuota::not_configured(
                PROVIDER,
                "Install the optional Browser Bridge to add Gemini.",
            );
        }
        Err(_) => return needs_browser(),
    };
    browser_quota_from_cache(&raw, now()).unwrap_or_else(needs_browser)
}

fn browser_quota_from_cache(raw: &str, current_time: i64) -> Option<ProviderQuota> {
    let cache: BrowserCache = serde_json::from_str(raw).ok()?;
    if cache.version != 1
        || cache.provider != PROVIDER
        || cache.payload.account_id.is_empty()
        || !cache
            .payload
            .account_id
            .chars()
            .all(|char| char.is_ascii_digit())
    {
        return None;
    }

    let age = current_time - cache.observed_at;
    if !(-CLOCK_SKEW_SECONDS..=CACHE_MAX_AGE_SECONDS).contains(&age) {
        return None;
    }

    let mut windows = Vec::new();
    add_window(
        &mut windows,
        "Session (5h)",
        cache.payload.ratio5h,
        cache.payload.reset_time5h,
        5 * 60 * 60,
        current_time,
    )?;
    add_window(
        &mut windows,
        "Weekly",
        cache.payload.ratio7d,
        cache.payload.reset_time7d,
        7 * 24 * 60 * 60,
        current_time,
    )?;
    windows.retain(|window| window.resets_at.is_none_or(|reset| reset > current_time));
    if windows.is_empty() {
        return None;
    }

    let account_number = cache
        .payload
        .account_id
        .parse::<u64>()
        .ok()?
        .checked_add(1)?;
    let plan = Some(match cache.payload.tier {
        Some(1) => "Free".to_string(),
        Some(2) => "AI Pro".to_string(),
        _ => format!("Account {account_number}"),
    });
    if age <= FRESH_MAX_AGE_SECONDS {
        Some(ProviderQuota::ok(PROVIDER, plan, windows))
    } else {
        Some(ProviderQuota::ok_stale(
            PROVIDER,
            plan,
            windows,
            Stale {
                source: "Gemini browser extension".to_string(),
                observed_at: Some(cache.observed_at),
                reason: "No recent Gemini browser push is available.".to_string(),
            },
        ))
    }
}

fn add_window(
    windows: &mut Vec<QuotaWindow>,
    label: &str,
    ratio: f64,
    resets_at: Option<i64>,
    window_seconds: i64,
    current_time: i64,
) -> Option<()> {
    if !ratio.is_finite() || !(0.0..=1.0).contains(&ratio) {
        return None;
    }
    if resets_at.is_none_or(|reset| reset > current_time) {
        windows.push(QuotaWindow {
            label: label.to_string(),
            percent: ratio * 100.0,
            resets_at,
            window_seconds: Some(window_seconds),
            severity: None,
        });
    }
    Some(())
}

fn needs_browser() -> ProviderQuota {
    ProviderQuota::action_required(
        PROVIDER,
        "Open Gemini in a browser once so its extension can update this card.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(observed_at: i64, reset_5h: i64, reset_7d: i64) -> String {
        serde_json::json!({
            "version": 1,
            "provider": "gemini",
            "observed_at": observed_at,
            "payload": {
                "account_id": "3",
                "tier": 2,
                "remaining5h": 1200,
                "ratio5h": 0.5,
                "resetTime5h": reset_5h,
                "remaining7d": 24000,
                "ratio7d": 0.25,
                "resetTime7d": reset_7d
            }
        })
        .to_string()
    }

    #[test]
    fn parses_account_namespaced_usage_without_mixing_units() {
        let ProviderQuota::Ok {
            plan,
            windows,
            stale,
            ..
        } = browser_quota_from_cache(&cache(1000, 2000, 3000), 1100).unwrap()
        else {
            panic!("fresh Gemini cache must produce a quota card");
        };
        assert_eq!(plan.as_deref(), Some("AI Pro"));
        assert!(stale.is_none());
        assert_eq!(windows[0].label, "Session (5h)");
        assert_eq!(windows[0].percent, 50.0);
        assert_eq!(windows[1].label, "Weekly");
        assert_eq!(windows[1].percent, 25.0);
    }

    #[test]
    fn labels_an_old_but_still_valid_snapshot_as_cached() {
        let current_time = 1000 + FRESH_MAX_AGE_SECONDS + 1;
        let ProviderQuota::Ok { stale, .. } = browser_quota_from_cache(
            &cache(1000, current_time + 60, current_time + 120),
            current_time,
        )
        .unwrap() else {
            panic!("old valid Gemini cache must remain visible");
        };
        assert_eq!(stale.unwrap().observed_at, Some(1000));
    }

    #[test]
    fn drops_an_expired_session_but_keeps_the_weekly_window() {
        let ProviderQuota::Ok { windows, .. } =
            browser_quota_from_cache(&cache(1000, 1099, 3000), 1100).unwrap()
        else {
            panic!("the unexpired weekly window should remain");
        };
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "Weekly");
    }

    #[test]
    fn rejects_expired_or_implausibly_old_snapshots() {
        assert!(browser_quota_from_cache(&cache(1000, 1099, 1099), 1100).is_none());
        assert!(browser_quota_from_cache(
            &cache(1000, 9_999_999, 9_999_999),
            1000 + CACHE_MAX_AGE_SECONDS + 1,
        )
        .is_none());
    }

    #[test]
    fn rejects_partial_or_out_of_range_usage() {
        let raw = serde_json::json!({
            "version": 1,
            "provider": "gemini",
            "observed_at": 1000,
            "payload": {
                "account_id": "0",
                "ratio5h": 1.2,
                "ratio7d": 0.25
            }
        })
        .to_string();
        assert!(browser_quota_from_cache(&raw, 1100).is_none());
    }

    #[test]
    fn unknown_tiers_use_a_human_account_number() {
        let raw = cache(1000, 2000, 3000).replace("\"tier\":2", "\"tier\":99");
        let ProviderQuota::Ok { plan, .. } = browser_quota_from_cache(&raw, 1100).unwrap() else {
            panic!("unknown tier should still produce a quota card");
        };
        assert_eq!(plan.as_deref(), Some("Account 4"));
    }
}
