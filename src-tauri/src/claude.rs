//! Claude quota, read from the OAuth token Claude Code leaves on disk.
//! See ARCHITECTURE.md §3.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use serde::{Deserialize, Serialize};

use crate::quota::{http_failure, now, rfc3339_to_unix, ProviderQuota, QuotaWindow, Stale};

const PROVIDER: &str = "claude";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// The usage response says nothing about which plan produced it. The plan lives
/// here instead.
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
const ANTHROPIC_BETA: &str = "oauth-2025-04-20";
const FALLBACK_CLAUDE_VERSION: &str = "2.1.204";
const REQUEST_TIMEOUT_SECONDS: u64 = 10;
const TOKEN_REFRESH_TIMEOUT_SECONDS: u64 = 60;
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A plan changes when someone upgrades and at no other time, so it is worth one
/// request every few hours rather than one per poll — this endpoint family
/// answers 429 when pushed.
const PLAN_TTL_SECONDS: i64 = 6 * 3600;
/// A failed lookup is retried sooner than a good one is refreshed, but not so
/// soon that a permanently unavailable profile costs a request every poll.
const PLAN_RETRY_SECONDS: i64 = 30 * 60;
/// A failed live request may reuse the last quota-only snapshot. Keep this
/// bounded: an old weekly figure is better than a blank card during a brief
/// outage, but not after a full day of missed observations.
const SNAPSHOT_MAX_AGE_SECONDS: i64 = 24 * 60 * 60;
/// A 429 without Retry-After still needs to be distinguishable from a cached
/// success so the frontend does not accidentally clear its backoff state.
const RATE_LIMIT_MIN_RETRY_SECONDS: u64 = 3 * 60;
const RATE_LIMIT_MAX_RETRY_SECONDS: u64 = 15 * 60;

static PLAN_CACHE: Mutex<Option<CachedPlan>> = Mutex::new(None);
static CLAUDE_USER_AGENT: OnceLock<String> = OnceLock::new();

struct CachedPlan {
    plan: Option<String>,
    at: i64,
}

struct FetchError {
    message: String,
    retry_after_seconds: Option<u64>,
    rate_limited: bool,
}

impl FetchError {
    fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retry_after_seconds: None,
            rate_limited: false,
        }
    }

    fn rate_limited(message: String, retry_after_seconds: Option<u64>) -> Self {
        Self {
            message,
            retry_after_seconds,
            rate_limited: true,
        }
    }

    fn auth_rejected() -> Self {
        Self {
            message: "Claude Code could not refresh the rejected token automatically — open Claude Code once and try again".to_string(),
            retry_after_seconds: None,
            rate_limited: false,
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
struct RateLimitState {
    until: i64,
    failures: u32,
}

#[derive(Deserialize, Serialize)]
struct CachedUsage {
    plan: Option<String>,
    windows: Vec<QuotaWindow>,
    observed_at: i64,
}

#[derive(Deserialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    oauth: Oauth,
}

/// Note what is *not* read here: `subscriptionType`. It reported `pro` on a
/// Claude Max account, so it cannot be trusted to name the plan (§3).
#[derive(Deserialize)]
struct Oauth {
    #[serde(rename = "accessToken")]
    access_token: String,
}

#[derive(Deserialize)]
struct Profile {
    #[serde(default)]
    organization: Option<Organization>,
}

#[derive(Deserialize)]
struct Organization {
    #[serde(default)]
    organization_type: Option<String>,
    #[serde(default)]
    rate_limit_tier: Option<String>,
}

#[derive(Deserialize)]
struct UsageResponse {
    /// The normalised, self-describing list. The flat `five_hour` /
    /// `seven_day_opus` / `seven_day_cowork` fields alongside it are the older
    /// interface and are `null` on plans without those buckets, so they are
    /// deliberately not read here.
    #[serde(default)]
    limits: Vec<Limit>,
}

#[derive(Deserialize)]
struct Limit {
    kind: String,
    percent: f64,
    #[serde(default)]
    severity: Option<String>,
    #[serde(default)]
    resets_at: Option<String>,
    #[serde(default)]
    scope: Option<Scope>,
}

#[derive(Deserialize)]
struct Scope {
    #[serde(default)]
    model: Option<ScopeModel>,
}

#[derive(Deserialize)]
struct ScopeModel {
    #[serde(default)]
    display_name: Option<String>,
}

fn credentials_path() -> Option<PathBuf> {
    // Claude Code honours CLAUDE_CONFIG_DIR, so anything reading its files has to.
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join(".credentials.json"));
    }
    dirs::home_dir().map(|home| home.join(".claude").join(".credentials.json"))
}

fn parse_cli_version(output: &str) -> Option<String> {
    let version = output.split_whitespace().next()?;
    let parts: Vec<_> = version.split('.').collect();
    (parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())))
    .then(|| version.to_string())
}

fn claude_cli_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            candidates.push(dir.join("claude.exe"));
            candidates.push(dir.join("claude.cmd"));
        }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let npm = PathBuf::from(appdata).join("npm");
        candidates.push(npm.join("claude.exe"));
        candidates.push(npm.join("claude.cmd"));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local").join("bin").join("claude.exe"));
    }
    candidates
}

fn claude_command(path: &std::path::Path) -> Command {
    let is_cmd = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd"));
    if is_cmd {
        let mut command = Command::new("cmd.exe");
        command.args(["/D", "/C"]).arg(path);
        command
    } else {
        Command::new(path)
    }
}

fn cli_version(path: &std::path::Path) -> Option<String> {
    let mut command = claude_command(path);
    command
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = command.spawn().ok()?;
    let started = Instant::now();
    loop {
        if child.try_wait().ok()?.is_some() {
            let output = child.wait_with_output().ok()?;
            return parse_cli_version(&String::from_utf8_lossy(&output.stdout));
        }
        if started.elapsed() >= Duration::from_secs(REQUEST_TIMEOUT_SECONDS) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Ask the official Claude Code CLI to rotate its own OAuth access token.
///
/// The deck never reads or spends the refresh token. `claude update` owns that
/// credential lifecycle; after it exits, the only success signal we trust is a
/// changed access token in Claude Code's credential file. All command output is
/// discarded so neither provider details nor local paths reach the deck UI.
fn refresh_access_token(rejected_token: &str) -> bool {
    let Some(path) = claude_cli_candidates()
        .into_iter()
        .find(|path| path.is_file())
    else {
        return false;
    };

    let mut command = claude_command(&path);
    command
        .arg("update")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);

    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < Duration::from_secs(TOKEN_REFRESH_TIMEOUT_SECONDS) => {
                std::thread::sleep(Duration::from_millis(100));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }

    read_credentials().is_ok_and(|credentials| credentials.oauth.access_token != rejected_token)
}

fn read_credentials() -> Result<Credentials, FetchError> {
    let path = credentials_path()
        .ok_or_else(|| FetchError::plain("could not locate the home directory"))?;
    let raw = std::fs::read_to_string(&path).map_err(|error| {
        FetchError::plain(format!(
            "cannot read {} — is Claude Code installed and signed in? ({error})",
            path.display()
        ))
    })?;
    // serde_json errors carry a position, not file content, so this is safe to
    // surface. Nothing below may ever interpolate the token itself.
    serde_json::from_str(&raw).map_err(|error| {
        FetchError::plain(format!("unexpected shape in .credentials.json: {error}"))
    })
}

fn claude_user_agent() -> &'static str {
    CLAUDE_USER_AGENT
        .get_or_init(|| {
            let version = claude_cli_candidates()
                .into_iter()
                .filter(|path| path.is_file())
                .find_map(|path| cli_version(&path))
                .unwrap_or_else(|| FALLBACK_CLAUDE_VERSION.to_string());
            format!("claude-code/{version}")
        })
        .as_str()
}

fn claude_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECONDS))
        .build()
}

fn api_request(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    user_agent: &str,
) -> reqwest::RequestBuilder {
    client
        .get(url)
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::USER_AGENT, user_agent)
        .header("anthropic-beta", ANTHROPIC_BETA)
}

fn provider_cache_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join("ai-quota-deck").join("provider-cache"))
}

fn usage_cache_path() -> Option<PathBuf> {
    provider_cache_dir().map(|dir| dir.join("claude.json"))
}

fn rate_limit_cache_path() -> Option<PathBuf> {
    provider_cache_dir().map(|dir| dir.join("claude-rate-limit.json"))
}

fn read_rate_limit_state() -> RateLimitState {
    rate_limit_cache_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_rate_limit_state(state: &RateLimitState) -> Result<(), String> {
    let path =
        rate_limit_cache_path().ok_or("could not locate the local application data directory")?;
    let parent = path
        .parent()
        .ok_or("Claude rate-limit cache path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let bytes = serde_json::to_vec(state)
        .map_err(|error| format!("cannot serialize Claude rate-limit state: {error}"))?;
    std::fs::write(&path, bytes)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn rate_limit_remaining(state: &RateLimitState, current_time: i64) -> Option<u64> {
    let remaining = state.until.checked_sub(current_time)?;
    (remaining > 0).then_some(remaining as u64)
}

fn next_rate_limit_state(
    previous: &RateLimitState,
    current_time: i64,
    retry_after_seconds: Option<u64>,
) -> (RateLimitState, u64) {
    let failures = previous.failures.saturating_add(1);
    let exponent = failures.saturating_sub(1).min(3);
    let exponential = RATE_LIMIT_MIN_RETRY_SECONDS
        .saturating_mul(1_u64 << exponent)
        .min(RATE_LIMIT_MAX_RETRY_SECONDS);
    let delay = retry_after_seconds
        .map(|seconds| seconds.clamp(RATE_LIMIT_MIN_RETRY_SECONDS, RATE_LIMIT_MAX_RETRY_SECONDS))
        .unwrap_or(exponential);
    (
        RateLimitState {
            until: current_time.saturating_add(delay as i64),
            failures,
        },
        delay,
    )
}

fn record_rate_limit(retry_after_seconds: Option<u64>) -> u64 {
    let current_time = now();
    let (state, delay) =
        next_rate_limit_state(&read_rate_limit_state(), current_time, retry_after_seconds);
    let _ = write_rate_limit_state(&state);
    delay
}

fn clear_rate_limit() {
    let state = read_rate_limit_state();
    if state.until != 0 || state.failures != 0 {
        let _ = write_rate_limit_state(&RateLimitState::default());
    }
}

fn write_cached_usage(plan: &Option<String>, windows: &[QuotaWindow]) -> Result<(), String> {
    let path = usage_cache_path().ok_or("could not locate the local application data directory")?;
    let parent = path
        .parent()
        .ok_or("Claude cache path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let snapshot = CachedUsage {
        plan: plan.clone(),
        windows: windows.to_vec(),
        observed_at: now(),
    };
    let bytes = serde_json::to_vec(&snapshot)
        .map_err(|error| format!("cannot serialize Claude cache: {error}"))?;
    std::fs::write(&path, bytes)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn cached_quota_from_raw(raw: &str, current_time: i64, reason: &str) -> Option<ProviderQuota> {
    let mut snapshot: CachedUsage = serde_json::from_str(raw).ok()?;
    let age = current_time.checked_sub(snapshot.observed_at)?;
    if snapshot.observed_at <= 0 || !(0..=SNAPSHOT_MAX_AGE_SECONDS).contains(&age) {
        return None;
    }

    // Never carry a spent session window across its reset. A still-valid weekly
    // row may remain useful while the live endpoint cools down.
    snapshot
        .windows
        .retain(|window| window.resets_at.is_none_or(|reset| reset > current_time));
    if snapshot.windows.is_empty() {
        return None;
    }

    Some(ProviderQuota::ok_stale(
        PROVIDER,
        snapshot.plan,
        snapshot.windows,
        Stale {
            source: "last successful Claude check".to_string(),
            observed_at: Some(snapshot.observed_at),
            reason: reason.to_string(),
        },
    ))
}

fn cached_quota(reason: &str) -> Option<ProviderQuota> {
    let raw = std::fs::read_to_string(usage_cache_path()?).ok()?;
    cached_quota_from_raw(&raw, now(), reason)
}

/// An unrecognised `kind` is passed through rather than dropped, so a new bucket
/// Anthropic adds shows up as an oddly-named card instead of silently vanishing.
fn label(limit: &Limit) -> String {
    let base = match limit.kind.as_str() {
        "session" => "Session",
        "weekly_all" => "Weekly",
        "weekly_scoped" => "Weekly",
        other => other,
    };

    match limit
        .scope
        .as_ref()
        .and_then(|s| s.model.as_ref())
        .and_then(|m| m.display_name.as_deref())
    {
        Some(model) => format!("{base} · {model}"),
        None => base.to_string(),
    }
}

fn window_seconds(kind: &str) -> Option<i64> {
    match kind {
        "session" => Some(5 * 60 * 60),
        "weekly_all" | "weekly_scoped" => Some(7 * 24 * 60 * 60),
        _ => None,
    }
}

fn titlecase(words: &str) -> String {
    words
        .split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `default_claude_max_5x` → `5x`. Only a trailing `<digits>x` segment counts;
/// anything else is not a multiplier and is left off rather than invented.
fn multiplier(rate_limit_tier: &str) -> Option<&str> {
    let last = rate_limit_tier.rsplit('_').next()?;
    let digits = last.strip_suffix('x')?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then_some(last)
}

/// `claude_max` + `default_claude_max_5x` → `Max 5x`.
///
/// Both halves come from the provider: `organization_type` names the tier and
/// `rate_limit_tier` carries the multiplier where one exists. An unfamiliar tier
/// keeps its own words instead of being mapped onto a known one.
fn plan_label(organization_type: &str, rate_limit_tier: Option<&str>) -> String {
    let base = titlecase(
        organization_type
            .strip_prefix("claude_")
            .unwrap_or(organization_type),
    );
    match rate_limit_tier.and_then(multiplier) {
        Some(times) => format!("{base} {times}"),
        None => base,
    }
}

/// Never fails the card: the plan is a label on an otherwise complete reading,
/// so a profile lookup that does not work simply leaves it off.
async fn fetch_plan(token: &str) -> Option<String> {
    let client = claude_client().ok()?;
    let response = api_request(&client, PROFILE_URL, token, claude_user_agent())
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let organization = response.json::<Profile>().await.ok()?.organization?;
    let kind = organization.organization_type?;
    Some(plan_label(&kind, organization.rate_limit_tier.as_deref()))
}

async fn plan(token: &str) -> Option<String> {
    // Read and release the lock before any await — a MutexGuard must not be
    // held across one.
    let cached = PLAN_CACHE
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|c| (c.plan.clone(), c.at)));

    if let Some((plan, at)) = cached {
        let ttl = if plan.is_some() {
            PLAN_TTL_SECONDS
        } else {
            PLAN_RETRY_SECONDS
        };
        if now() - at < ttl {
            return plan;
        }
    }

    let fetched = fetch_plan(token).await;
    if let Ok(mut guard) = PLAN_CACHE.lock() {
        *guard = Some(CachedPlan {
            plan: fetched.clone(),
            at: now(),
        });
    }
    fetched
}

pub async fn fetch() -> ProviderQuota {
    let Some(path) = credentials_path() else {
        return ProviderQuota::error(PROVIDER, "could not locate the home directory");
    };
    if matches!(path.try_exists(), Ok(false)) {
        return ProviderQuota::not_configured(
            PROVIDER,
            "Run Claude Code and sign in to add Claude.",
        );
    }

    let rate_limit_state = read_rate_limit_state();
    if let Some(remaining) = rate_limit_remaining(&rate_limit_state, now()) {
        let message = http_failure(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "the Claude usage endpoint",
        );
        return cached_quota(&message)
            .unwrap_or_else(|| ProviderQuota::error(PROVIDER, message))
            .with_retry_after(Some(remaining));
    }

    match try_fetch().await {
        Ok(quota) => {
            clear_rate_limit();
            quota
        }
        Err(failure) => {
            let retry_after_seconds = failure
                .rate_limited
                .then(|| record_rate_limit(failure.retry_after_seconds));
            let quota = cached_quota(&failure.message)
                .unwrap_or_else(|| ProviderQuota::error(PROVIDER, failure.message));
            quota.with_retry_after(retry_after_seconds)
        }
    }
}

async fn try_fetch() -> Result<ProviderQuota, FetchError> {
    let mut creds = read_credentials()?;
    let client = claude_client()
        .map_err(|error| FetchError::plain(format!("could not build HTTP client: {error}")))?;
    let mut automatic_refresh_attempted = false;
    let body: UsageResponse = loop {
        let response = api_request(
            &client,
            USAGE_URL,
            &creds.oauth.access_token,
            claude_user_agent(),
        )
        .send()
        .await
        .map_err(|error| {
            FetchError::plain(format!("request to the usage endpoint failed: {error}"))
        })?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            if automatic_refresh_attempted {
                return Err(FetchError::auth_rejected());
            }
            automatic_refresh_attempted = true;
            let rejected_token = creds.oauth.access_token.clone();
            let refreshed =
                tauri::async_runtime::spawn_blocking(move || refresh_access_token(&rejected_token))
                    .await
                    .unwrap_or(false);
            if !refreshed {
                return Err(FetchError::auth_rejected());
            }
            creds = read_credentials()?;
            continue;
        }
        if !response.status().is_success() {
            let status = response.status();
            let retry_after_seconds = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok());
            let message = http_failure(status, "the Claude usage endpoint");
            return Err(if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                FetchError::rate_limited(message, retry_after_seconds)
            } else {
                FetchError::plain(message)
            });
        }

        break response.json().await.map_err(|error| {
            FetchError::plain(format!("could not parse the usage response: {error}"))
        })?;
    };

    if body.limits.is_empty() {
        // The call worked; there is simply nothing metered on this plan.
        return Ok(ProviderQuota::unavailable(
            PROVIDER,
            "Signed in, but this plan reports no usage limits.",
        ));
    }

    let windows: Vec<QuotaWindow> = body
        .limits
        .iter()
        .map(|limit| QuotaWindow {
            label: label(limit),
            percent: limit.percent,
            resets_at: limit.resets_at.as_deref().and_then(rfc3339_to_unix),
            window_seconds: window_seconds(&limit.kind),
            severity: limit.severity.clone(),
        })
        .collect();

    // Only after the usage call succeeded, so a rate-limited account does not
    // get a second request piled on top.
    let plan = plan(&creds.oauth.access_token).await;

    // Best-effort and quota-only. A cache write failure must never hide a live
    // reading, and no credential or raw provider response reaches this file.
    let _ = write_cached_usage(&plan, &windows);

    Ok(ProviderQuota::ok(PROVIDER, plan, windows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_plan_with_its_multiplier() {
        assert_eq!(
            plan_label("claude_max", Some("default_claude_max_5x")),
            "Max 5x"
        );
        assert_eq!(
            plan_label("claude_max", Some("default_claude_max_20x")),
            "Max 20x"
        );
    }

    #[test]
    fn leaves_the_multiplier_off_when_there_is_none() {
        assert_eq!(plan_label("claude_pro", Some("default_claude_pro")), "Pro");
        assert_eq!(plan_label("claude_pro", None), "Pro");
        // `default_claude_ai` ends in a word, not a multiplier.
        assert_eq!(plan_label("claude_team", Some("default_claude_ai")), "Team");
    }

    #[test]
    fn passes_an_unfamiliar_tier_through() {
        assert_eq!(
            plan_label("claude_enterprise_trial", None),
            "Enterprise Trial"
        );
        assert_eq!(plan_label("something_else", None), "Something Else");
    }

    #[test]
    fn only_a_trailing_digits_x_counts_as_a_multiplier() {
        assert_eq!(multiplier("default_claude_max_5x"), Some("5x"));
        assert_eq!(multiplier("default_claude_ai"), None);
        assert_eq!(multiplier("tier_x"), None);
        assert_eq!(multiplier("tier_5"), None);
    }

    #[test]
    fn known_limit_kinds_supply_their_pace_window() {
        assert_eq!(window_seconds("session"), Some(5 * 60 * 60));
        assert_eq!(window_seconds("weekly_all"), Some(7 * 24 * 60 * 60));
        assert_eq!(window_seconds("weekly_scoped"), Some(7 * 24 * 60 * 60));
        assert_eq!(window_seconds("new_kind"), None);
    }

    #[test]
    fn a_failed_live_check_reuses_only_unexpired_cached_windows() {
        let raw = serde_json::json!({
            "plan": "Max 5x",
            "observed_at": 1_000,
            "windows": [
                {
                    "label": "Session",
                    "percent": 80.0,
                    "resets_at": 1_050,
                    "window_seconds": 18_000,
                    "severity": "normal"
                },
                {
                    "label": "Weekly",
                    "percent": 20.0,
                    "resets_at": 5_000,
                    "window_seconds": 604_800,
                    "severity": "normal"
                }
            ]
        })
        .to_string();

        let ProviderQuota::Ok {
            plan,
            windows,
            stale: Some(stale),
            ..
        } = cached_quota_from_raw(&raw, 1_100, "rate limited").unwrap()
        else {
            panic!("a recent cached quota should stay visible");
        };
        assert_eq!(plan.as_deref(), Some("Max 5x"));
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "Weekly");
        assert_eq!(stale.observed_at, Some(1_000));
        assert_eq!(stale.reason, "rate limited");
    }

    #[test]
    fn a_day_old_cached_check_is_not_presented_as_current() {
        let raw = serde_json::json!({
            "plan": "Max 5x",
            "observed_at": 1_000,
            "windows": [{
                "label": "Weekly",
                "percent": 20.0,
                "resets_at": 200_000,
                "window_seconds": 604_800,
                "severity": "normal"
            }]
        })
        .to_string();

        assert!(
            cached_quota_from_raw(&raw, 1_000 + SNAPSHOT_MAX_AGE_SECONDS + 1, "rate limited")
                .is_none()
        );
    }

    #[test]
    fn claude_requests_match_the_working_monitors_headers() {
        let client = claude_client().unwrap();
        let request = api_request(&client, USAGE_URL, "fake-test-token", "claude-code/9.8.7")
            .build()
            .unwrap();
        assert_eq!(
            request.headers()[reqwest::header::AUTHORIZATION],
            "Bearer fake-test-token"
        );
        assert_eq!(
            request.headers()[reqwest::header::CONTENT_TYPE],
            "application/json"
        );
        assert_eq!(
            request.headers()[reqwest::header::USER_AGENT],
            "claude-code/9.8.7"
        );
        assert_eq!(request.headers()["anthropic-beta"], ANTHROPIC_BETA);
    }

    #[test]
    fn parses_only_a_leading_three_part_cli_version() {
        assert_eq!(
            parse_cli_version("2.1.225 (Claude Code)\r\n").as_deref(),
            Some("2.1.225")
        );
        assert_eq!(parse_cli_version("Claude 2.1.225"), None);
        assert_eq!(parse_cli_version("2.1"), None);
    }

    #[test]
    fn rate_limit_state_survives_restart_and_escalates() {
        let (first, first_delay) = next_rate_limit_state(&RateLimitState::default(), 1_000, None);
        assert_eq!(first_delay, 180);
        assert_eq!(rate_limit_remaining(&first, 1_100), Some(80));
        assert_eq!(rate_limit_remaining(&first, 1_180), None);

        let (second, second_delay) = next_rate_limit_state(&first, 1_180, None);
        assert_eq!(second.failures, 2);
        assert_eq!(second_delay, 360);

        let (_, short_header) = next_rate_limit_state(&second, 1_540, Some(30));
        assert_eq!(short_header, 180);
        let (_, long_header) = next_rate_limit_state(&second, 1_540, Some(3_600));
        assert_eq!(long_header, 900);
    }
}
