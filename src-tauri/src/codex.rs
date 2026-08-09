//! Codex quota. Live from the OAuth token the Codex app leaves on disk, with
//! the local session logs as a labelled fallback. See ARCHITECTURE.md §4.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::quota::{
    http_failure, label_for_window_minutes, rfc3339_to_unix, ProviderQuota, QuotaWindow, Stale,
};

const PROVIDER: &str = "codex";
/// `wham` is OpenAI's internal name for the Codex backend. `/backend-api/codex/usage`
/// is an alias returning the same bytes; `/backend-api/api/codex/usage` is a 404.
const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

/// Old Codex builds wrote `token_count` events with no `rate_limits` key at all,
/// so a machine with a long history could have many useless files. Give up
/// rather than walk an unbounded archive.
const MAX_SESSION_FILES_SCANNED: usize = 20;

fn codex_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(dir));
    }
    dirs::home_dir().map(|home| home.join(".codex"))
}

fn plan_label(plan: Option<String>) -> Option<String> {
    plan.map(|value| {
        if value.eq_ignore_ascii_case("plus") {
            "Plus".to_string()
        } else {
            value
        }
    })
}

// ── Live endpoint ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Auth {
    tokens: Tokens,
}

#[derive(Deserialize)]
struct Tokens {
    access_token: String,
}

#[derive(Deserialize)]
struct UsageResponse {
    #[serde(default)]
    plan_type: Option<String>,
    rate_limit: RateLimit,
}

#[derive(Deserialize)]
struct RateLimit {
    #[serde(default)]
    primary_window: Option<LiveWindow>,
    #[serde(default)]
    secondary_window: Option<LiveWindow>,
}

#[derive(Deserialize)]
struct LiveWindow {
    used_percent: f64,
    #[serde(default)]
    limit_window_seconds: Option<f64>,
    #[serde(default)]
    reset_at: Option<i64>,
}

// ── Session-log fallback ────────────────────────────────────────────────────
//
// Same underlying numbers, different serialisation. Every field is spelled
// differently from the live response — `primary` not `primary_window`,
// `resets_at` not `reset_at`, `window_minutes` not `limit_window_seconds` —
// which is why these are separate types rather than one shared shape.

#[derive(Deserialize)]
struct RolloutLine {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    payload: Option<RolloutPayload>,
}

#[derive(Deserialize)]
struct RolloutPayload {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    rate_limits: Option<Snapshot>,
}

#[derive(Deserialize)]
struct Snapshot {
    #[serde(default)]
    primary: Option<SnapshotWindow>,
    #[serde(default)]
    secondary: Option<SnapshotWindow>,
    #[serde(default)]
    plan_type: Option<String>,
}

#[derive(Deserialize)]
struct SnapshotWindow {
    used_percent: f64,
    #[serde(default)]
    window_minutes: Option<f64>,
    #[serde(default)]
    resets_at: Option<i64>,
}

// ── Fetch ───────────────────────────────────────────────────────────────────

pub async fn fetch() -> ProviderQuota {
    let auth_missing = codex_home()
        .map(|home| home.join("auth.json"))
        .is_some_and(|path| matches!(path.try_exists(), Ok(false)));

    match live().await {
        Ok(quota) => quota,
        Err(reason) => match from_session_logs(&reason) {
            Some(quota) => quota,
            None if auth_missing => ProviderQuota::not_configured(
                PROVIDER,
                "Open the Codex app and sign in to add Codex.",
            ),
            None => ProviderQuota::error(PROVIDER, reason),
        },
    }
}

async fn live() -> Result<ProviderQuota, String> {
    let path = codex_home()
        .ok_or("could not locate the home directory")?
        .join("auth.json");
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "cannot read {} — is the Codex app installed and signed in? ({e})",
            path.display()
        )
    })?;
    let auth: Auth =
        serde_json::from_str(&raw).map_err(|e| format!("unexpected shape in auth.json: {e}"))?;

    let response = reqwest::Client::new()
        .get(USAGE_URL)
        .bearer_auth(&auth.tokens.access_token)
        .send()
        .await
        .map_err(|e| format!("request to the usage endpoint failed: {e}"))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err("token rejected — open the Codex app once so it can refresh".into());
    }
    if !response.status().is_success() {
        return Err(http_failure(response.status(), "the Codex usage endpoint"));
    }

    let body: UsageResponse = response
        .json()
        .await
        .map_err(|e| format!("could not parse the usage response: {e}"))?;

    let windows = windows_from(&body.rate_limit);

    if windows.is_empty() {
        // Returned rather than handed to the session-log fallback on purpose:
        // the account currently has no window, so older local numbers would
        // show a quota that no longer applies.
        return Ok(ProviderQuota::unavailable(
            PROVIDER,
            "Signed in, but this plan reports no usage window.",
        ));
    }

    Ok(ProviderQuota::ok(
        PROVIDER,
        plan_label(body.plan_type),
        windows,
    ))
}

/// Both slots are optional and neither implies a period — a Plus account was
/// measured with a weekly `primary_window` and nothing in `secondary_window`.
/// See §7: the label comes from the reported duration, never from the position.
fn windows_from(rate_limit: &RateLimit) -> Vec<QuotaWindow> {
    [&rate_limit.primary_window, &rate_limit.secondary_window]
        .into_iter()
        .flatten()
        .map(|w| QuotaWindow {
            label: w
                .limit_window_seconds
                .map(|seconds| label_for_window_minutes(seconds / 60.0))
                .unwrap_or_else(|| "Usage".to_string()),
            percent: w.used_percent,
            resets_at: w.reset_at,
            window_seconds: w.limit_window_seconds.map(|s| s as i64),
            // Codex reports no severity of its own.
            severity: None,
        })
        .collect()
}

/// Read the newest `rate_limits` Codex recorded locally.
///
/// This only advances when a Codex turn completes, and anything done on the web
/// never reaches it — hence `ok_stale`, never `ok`.
fn from_session_logs(reason: &str) -> Option<ProviderQuota> {
    let (snapshot, observed_at) = latest_snapshot()?;

    let windows: Vec<QuotaWindow> = [&snapshot.primary, &snapshot.secondary]
        .into_iter()
        .flatten()
        .map(|w| QuotaWindow {
            label: w
                .window_minutes
                .map(label_for_window_minutes)
                .unwrap_or_else(|| "Usage".to_string()),
            percent: w.used_percent,
            resets_at: w.resets_at,
            window_seconds: w.window_minutes.map(|m| (m * 60.0) as i64),
            severity: None,
        })
        .collect();

    if windows.is_empty() {
        return None;
    }

    Some(ProviderQuota::ok_stale(
        PROVIDER,
        plan_label(snapshot.plan_type),
        windows,
        Stale {
            source: "local Codex session log".to_string(),
            observed_at,
            reason: reason.to_string(),
        },
    ))
}

fn latest_snapshot() -> Option<(Snapshot, Option<i64>)> {
    let sessions = codex_home()?.join("sessions");
    let mut files = Vec::new();
    collect_jsonl(&sessions, &mut files);
    // Newest first: the most recent session is overwhelmingly the one that has
    // the field, so this normally reads exactly one file.
    files.sort_by_key(|entry| std::cmp::Reverse(entry.1));

    files
        .into_iter()
        .take(MAX_SESSION_FILES_SCANNED)
        .find_map(|(path, _)| last_snapshot_in(&path))
}

fn collect_jsonl(dir: &Path, out: &mut Vec<(PathBuf, std::time::SystemTime)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            out.push((path, modified));
        }
    }
}

/// The *last* matching event in the file, not the first — a session accumulates
/// `token_count` events as it runs, and only the final one is current.
fn last_snapshot_in(path: &Path) -> Option<(Snapshot, Option<i64>)> {
    let file = File::open(path).ok()?;
    let mut latest = None;

    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<RolloutLine>(line) else {
            continue;
        };
        if record.kind.as_deref() != Some("event_msg") {
            continue;
        }
        let Some(payload) = record.payload else {
            continue;
        };
        if payload.kind.as_deref() != Some("token_count") {
            continue;
        }
        if let Some(limits) = payload.rate_limits {
            let observed_at = record.timestamp.as_deref().and_then(rfc3339_to_unix);
            latest = Some((limits, observed_at));
        }
    }

    latest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("rollout-sample.jsonl")
    }

    #[test]
    fn takes_the_last_snapshot_not_the_first() {
        let (snapshot, observed_at) = last_snapshot_in(&fixture()).expect("a snapshot");

        // The file opens with a 0% snapshot and ends with a 37.5% one. Reading
        // the first match would report a quota that is hours out of date.
        let primary = snapshot.primary.expect("a primary window");
        assert_eq!(primary.used_percent, 37.5);
        assert_eq!(primary.window_minutes, Some(300.0));
        assert_eq!(snapshot.plan_type.as_deref(), Some("plus"));

        let secondary = snapshot.secondary.expect("a secondary window");
        assert_eq!(secondary.used_percent, 12.0);

        // 2026-01-01T00:05:00Z
        assert_eq!(observed_at, Some(1767225900));
    }

    /// Measured on a real Plus account on 2026-08-09. It disproved the earlier
    /// assumption that a paid plan always reports 5h + weekly: the only window
    /// present is a weekly one, and it arrives in the *primary* slot.
    #[test]
    fn a_plus_account_reports_one_weekly_window_in_the_primary_slot() {
        let raw = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join("fixtures")
                .join("codex-plus-usage.json"),
        )
        .expect("the Codex Plus fixture");
        let body: UsageResponse = serde_json::from_str(&raw).expect("a parseable usage response");

        assert_eq!(plan_label(body.plan_type).as_deref(), Some("Plus"));

        let windows = windows_from(&body.rate_limit);
        assert_eq!(windows.len(), 1, "the empty secondary slot must not appear");

        let weekly = &windows[0];
        assert_eq!(weekly.label, "Weekly");
        assert_eq!(weekly.percent, 23.5);
        assert_eq!(weekly.window_seconds, Some(604_800));
        assert_eq!(weekly.resets_at, Some(1_767_830_400));
    }

    /// The null siblings alongside `secondary_window` are fields the deck does
    /// not model. They must be ignored, not treated as a parse failure.
    #[test]
    fn unmodelled_null_limit_fields_do_not_break_parsing() {
        let raw = r#"{"plan_type":"plus","rate_limit":{
            "primary_window":{"used_percent":1.0,"limit_window_seconds":604800},
            "secondary_window":null,
            "additional_rate_limits":null,
            "code_review_rate_limit":null,
            "some_field_added_next_quarter":{"nested":true}}}"#;
        let body: UsageResponse = serde_json::from_str(raw).expect("a parseable usage response");
        assert_eq!(windows_from(&body.rate_limit).len(), 1);
    }

    #[test]
    fn codex_plus_uses_the_product_label_casing() {
        assert_eq!(
            plan_label(Some("plus".to_string())).as_deref(),
            Some("Plus")
        );
        assert_eq!(
            plan_label(Some("free".to_string())).as_deref(),
            Some("free")
        );
    }

    #[test]
    fn skips_unparseable_and_unrelated_lines() {
        // The fixture contains a malformed line, a non-`event_msg` record, an
        // `event_msg` of another type, and a `token_count` with no
        // `rate_limits`. None of them may abort the scan or be mistaken for a
        // snapshot — a single bad line must not blank the card.
        assert!(last_snapshot_in(&fixture()).is_some());
    }

    #[test]
    fn missing_file_is_not_a_panic() {
        assert!(last_snapshot_in(Path::new("no-such-rollout.jsonl")).is_none());
    }

    /// The production fallback path end to end: a Codex home with session logs
    /// but no `auth.json`. Uses `CODEX_HOME` so the real installation is never
    /// touched — which also exercises that the env override is honoured.
    #[test]
    fn falls_back_to_session_logs_when_auth_is_missing() {
        let home = std::env::temp_dir().join("ai-quota-deck-test-codex-home");
        let day = home.join("sessions").join("2026").join("01").join("01");
        std::fs::create_dir_all(&day).expect("temp codex home");
        std::fs::copy(fixture(), day.join("rollout-sample.jsonl")).expect("copy fixture");
        assert!(
            !home.join("auth.json").exists(),
            "fixture home must have no token"
        );

        std::env::set_var("CODEX_HOME", &home);
        let quota = from_session_logs("token missing").expect("a stale reading");
        std::env::remove_var("CODEX_HOME");

        let ProviderQuota::Ok {
            plan,
            windows,
            stale,
            ..
        } = quota
        else {
            panic!("expected a reading, not an error card");
        };

        assert_eq!(plan.as_deref(), Some("Plus"));

        // Both slots, each named from its own reported length — not from order.
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label, "Session (5h)");
        assert_eq!(windows[0].percent, 37.5);
        assert_eq!(windows[1].label, "Weekly");
        assert_eq!(windows[1].percent, 12.0);

        // The card must be able to say how old this is and why it is not live.
        let stale = stale.expect("fallback data must be flagged stale");
        assert_eq!(stale.observed_at, Some(1767225900));
        assert_eq!(stale.reason, "token missing");
        assert!(stale.source.contains("session log"));

        let _ = std::fs::remove_dir_all(&home);
    }
}
