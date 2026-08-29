//! Persisted Claude request gate: 429 cooldown and the six-minute floor.
//!
//! Disk shape is quota-only — deadline, attempt time, failure count, and the
//! credential-file generation. No tokens. See ARCHITECTURE.md §2.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::quota::atomic_write;

/// A 429 must never make the deck more aggressive than a healthy Claude poll.
pub const CLAUDE_REQUEST_FLOOR_SECONDS: u64 = 6 * 60;
/// Without Retry-After, open the circuit for 6, 12, 24, 48, then 60 minutes.
const RATE_LIMIT_MIN_RETRY_SECONDS: u64 = CLAUDE_REQUEST_FLOOR_SECONDS;
const RATE_LIMIT_MAX_RETRY_SECONDS: u64 = 60 * 60;
/// Only protect the local cache from a corrupt server value measured in years.
const RATE_LIMIT_SERVER_MAX_RETRY_SECONDS: u64 = 24 * 60 * 60;
/// Cross the advertised boundary safely and avoid phase-lock with other tools.
const RATE_LIMIT_RETRY_BUFFER_SECONDS: u64 = 5;
pub const RATE_LIMIT_POLICY_VERSION: u8 = 2;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct RateLimitState {
    pub policy_version: u8,
    pub until: i64,
    pub failures: u32,
    pub last_attempt_at: i64,
    pub credential_generation: i64,
}

pub fn credential_generation(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn provider_cache_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join("ai-quota-deck").join("provider-cache"))
}

fn rate_limit_cache_path() -> Option<PathBuf> {
    provider_cache_dir().map(|dir| dir.join("claude-rate-limit.json"))
}

fn load_rate_limit_state() -> RateLimitState {
    rate_limit_cache_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn credentials_changed(state: &RateLimitState, current: i64) -> bool {
    current > 0 && state.credential_generation > 0 && state.credential_generation != current
}

/// Old builds capped Retry-After at 15 minutes and inflated `failures` by
/// retrying early. On upgrade, keep a still-future deadline and drop the count.
/// A one-request bump after upgrade is preferable to a second state machine
/// for fields those builds never wrote.
fn migrate_rate_limit_state(
    mut state: RateLimitState,
    current_time: i64,
    generation: i64,
) -> (RateLimitState, bool) {
    let mut changed = false;

    if state.policy_version < RATE_LIMIT_POLICY_VERSION {
        if state.until <= current_time {
            state.until = 0;
        }
        state.failures = 0;
        state.last_attempt_at = 0;
        state.policy_version = RATE_LIMIT_POLICY_VERSION;
        changed = true;
    }

    if credentials_changed(&state, generation) {
        state = RateLimitState {
            policy_version: RATE_LIMIT_POLICY_VERSION,
            credential_generation: generation,
            ..RateLimitState::default()
        };
        changed = true;
    } else if generation > 0 && state.credential_generation != generation {
        state.credential_generation = generation;
        changed = true;
    }

    (state, changed)
}

/// Load, migrate if needed, and persist any migration. The bool is true when
/// the on-disk credential generation did not match the file we are about to
/// use, so the caller can drop a plan label that belonged to the old login.
pub fn read_rate_limit_state(current_time: i64, generation: i64) -> (RateLimitState, bool) {
    let loaded = load_rate_limit_state();
    let rotated = credentials_changed(&loaded, generation);
    let (state, changed) = migrate_rate_limit_state(loaded, current_time, generation);
    if changed {
        let _ = write_rate_limit_state(&state);
    }
    (state, rotated)
}

pub fn write_rate_limit_state(state: &RateLimitState) -> Result<(), String> {
    let path =
        rate_limit_cache_path().ok_or("could not locate the local application data directory")?;
    let parent = path
        .parent()
        .ok_or("Claude rate-limit cache path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let bytes = serde_json::to_vec(state)
        .map_err(|error| format!("cannot serialize Claude rate-limit state: {error}"))?;
    atomic_write(&path, &bytes)
}

pub fn rate_limit_remaining(state: &RateLimitState, current_time: i64) -> Option<u64> {
    let remaining = state.until.checked_sub(current_time)?;
    (remaining > 0).then_some(remaining as u64)
}

pub fn request_floor_remaining(state: &RateLimitState, current_time: i64) -> Option<u64> {
    let until = state
        .last_attempt_at
        .saturating_add(CLAUDE_REQUEST_FLOOR_SECONDS as i64);
    let remaining = until.checked_sub(current_time)?;
    (state.last_attempt_at > 0 && remaining > 0).then_some(remaining as u64)
}

pub fn next_rate_limit_state(
    previous: &RateLimitState,
    current_time: i64,
    retry_after_seconds: Option<u64>,
) -> (RateLimitState, u64) {
    let failures = previous.failures.saturating_add(1);
    let exponent = failures.saturating_sub(1).min(4);
    let fallback = RATE_LIMIT_MIN_RETRY_SECONDS
        .saturating_mul(1_u64 << exponent)
        .min(RATE_LIMIT_MAX_RETRY_SECONDS);
    let server_delay = retry_after_seconds
        .map(|seconds| {
            seconds.clamp(
                CLAUDE_REQUEST_FLOOR_SECONDS,
                RATE_LIMIT_SERVER_MAX_RETRY_SECONDS,
            )
        })
        .unwrap_or(0);
    let delay = fallback
        .max(server_delay)
        .saturating_add(RATE_LIMIT_RETRY_BUFFER_SECONDS);
    let deadline_delta = i64::try_from(delay).unwrap_or(i64::MAX);
    let mut state = previous.clone();
    state.policy_version = RATE_LIMIT_POLICY_VERSION;
    state.until = current_time.saturating_add(deadline_delta);
    state.failures = failures;
    (state, delay)
}

pub fn mark_attempt(state: &mut RateLimitState, current_time: i64, generation: i64) {
    state.policy_version = RATE_LIMIT_POLICY_VERSION;
    state.last_attempt_at = current_time;
    if generation > 0 {
        state.credential_generation = generation;
    }
    let _ = write_rate_limit_state(state);
}

pub fn record_rate_limit(
    state: &mut RateLimitState,
    current_time: i64,
    retry_after_seconds: Option<u64>,
) -> u64 {
    let (next, delay) = next_rate_limit_state(state, current_time, retry_after_seconds);
    *state = next;
    let _ = write_rate_limit_state(state);
    delay
}

pub fn clear_rate_limit(state: &mut RateLimitState, generation: i64) {
    state.policy_version = RATE_LIMIT_POLICY_VERSION;
    state.until = 0;
    state.failures = 0;
    if generation > 0 {
        state.credential_generation = generation;
    }
    let _ = write_rate_limit_state(state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_rate_limit_state_survives_restart_and_escalates() {
        let (first, first_delay) = next_rate_limit_state(&RateLimitState::default(), 1_000, None);
        assert_eq!(first_delay, 365);
        assert_eq!(rate_limit_remaining(&first, 1_100), Some(265));
        assert_eq!(rate_limit_remaining(&first, 1_365), None);

        let expected = [725, 1_445, 2_885, 3_605, 3_605];
        let mut state = first;
        let mut current_time = state.until;
        for (index, expected_delay) in expected.into_iter().enumerate() {
            let (next, delay) = next_rate_limit_state(&state, current_time, None);
            assert_eq!(next.failures, index as u32 + 2);
            assert_eq!(delay, expected_delay);
            current_time = next.until;
            state = next;
        }
    }

    #[test]
    fn server_retry_after_is_not_cut_down_to_the_fallback_ceiling() {
        let (_, observed) = next_rate_limit_state(&RateLimitState::default(), 1_000, Some(1_073));
        assert_eq!(observed, 1_078);

        let (_, hour) = next_rate_limit_state(&RateLimitState::default(), 1_000, Some(3_600));
        assert_eq!(hour, 3_605);

        let (_, short) = next_rate_limit_state(&RateLimitState::default(), 1_000, Some(30));
        assert_eq!(short, 365);

        let (_, corrupt) = next_rate_limit_state(&RateLimitState::default(), 1_000, Some(u64::MAX));
        assert_eq!(
            corrupt,
            RATE_LIMIT_SERVER_MAX_RETRY_SECONDS + RATE_LIMIT_RETRY_BUFFER_SECONDS
        );
    }

    #[test]
    fn a_later_429_keeps_escalating() {
        let (first, _) = next_rate_limit_state(&RateLimitState::default(), 1_000, Some(3_600));
        let (second, delay) = next_rate_limit_state(&first, first.until, None);
        assert_eq!(second.failures, 2);
        assert_eq!(delay, 725);
    }

    #[test]
    fn expired_legacy_state_is_cleared() {
        let legacy = RateLimitState {
            until: 1_900,
            failures: 5,
            ..RateLimitState::default()
        };
        let (migrated, changed) = migrate_rate_limit_state(legacy, 3_000, 0);
        assert!(changed);
        assert_eq!(migrated.until, 0);
        assert_eq!(migrated.failures, 0);
        assert_eq!(migrated.policy_version, RATE_LIMIT_POLICY_VERSION);
    }

    #[test]
    fn v022_state_keeps_an_active_deadline_and_drops_the_inflated_count() {
        let legacy: RateLimitState =
            serde_json::from_str(r#"{"until":3200,"failures":4}"#).unwrap();
        let (migrated, _) = migrate_rate_limit_state(legacy, 3_000, 0);
        assert_eq!(migrated.until, 3_200);
        assert_eq!(migrated.failures, 0);
        assert_eq!(migrated.last_attempt_at, 0);
    }

    #[test]
    fn a_new_credential_does_not_inherit_the_old_token_deadline() {
        let previous = RateLimitState {
            policy_version: RATE_LIMIT_POLICY_VERSION,
            until: 9_000,
            failures: 3,
            last_attempt_at: 1_000,
            credential_generation: 11,
        };
        let (migrated, changed) = migrate_rate_limit_state(previous, 2_000, 12);
        assert!(changed);
        assert_eq!(migrated.until, 0);
        assert_eq!(migrated.failures, 0);
        assert_eq!(migrated.last_attempt_at, 0);
        assert_eq!(migrated.credential_generation, 12);
    }

    #[test]
    fn request_floor_survives_a_frontend_restart() {
        let state = RateLimitState {
            policy_version: RATE_LIMIT_POLICY_VERSION,
            last_attempt_at: 1_000,
            ..RateLimitState::default()
        };
        assert_eq!(request_floor_remaining(&state, 1_100), Some(260));
        assert_eq!(request_floor_remaining(&state, 1_360), None);
    }

    #[test]
    fn current_policy_is_left_alone() {
        let current = RateLimitState {
            policy_version: RATE_LIMIT_POLICY_VERSION,
            until: 4_000,
            failures: 2,
            last_attempt_at: 1_000,
            credential_generation: 7,
        };
        let (migrated, changed) = migrate_rate_limit_state(current, 3_000, 7);
        assert!(!changed);
        assert_eq!(migrated.until, 4_000);
        assert_eq!(migrated.failures, 2);
        assert_eq!(migrated.last_attempt_at, 1_000);
    }
}
