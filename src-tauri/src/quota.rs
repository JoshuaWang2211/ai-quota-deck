use serde::{Deserialize, Serialize};
use std::path::Path;

/// One quota window as the dashboard renders it.
///
/// `label` always originates with the provider — either a period name it
/// declared outright, or one derived from a duration it reported. It is never
/// inferred from position. Providers hand back *slots*, and what lands in a slot
/// depends on the plan: on a free ChatGPT account the first Codex slot is a
/// 30-day window, so labelling it "5-hour" because it came first would be wrong
/// and would look entirely plausible. See ARCHITECTURE.md §8.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QuotaWindow {
    pub label: String,
    /// Percentage of the allowance consumed, 0–100.
    pub percent: f64,
    /// Unix seconds. `None` when the provider did not say.
    pub resets_at: Option<i64>,
    /// How long the whole window runs, in seconds, when the provider reports it.
    /// With this and `resets_at` the dashboard can show how far through the
    /// window you are, not just how much you have spent. Claude does not send a
    /// duration on the wire; claude.rs derives one from the window kind.
    pub window_seconds: Option<i64>,
    /// The provider's own severity hint, where it offers one. Preferred over
    /// thresholds invented here — the provider knows what "close to the limit"
    /// means for its own plan.
    pub severity: Option<String>,
}

/// A split of one window's usage across products or models.
///
/// These are parts of a single pool, not separate quotas. Grok reports how much
/// of one subscription went to chat, voice, coding and image generation;
/// rendering those as windows would imply four independent limits where there
/// is one.
#[derive(Debug, Clone, Serialize)]
pub struct UsageSlice {
    pub label: String,
    pub percent: f64,
}

/// Set when the numbers did not come from a live request. The dashboard must
/// show the age rather than present cached figures as current.
#[derive(Debug, Clone, Serialize)]
pub struct Stale {
    /// Where the numbers came from instead.
    pub source: String,
    /// Unix seconds — when the provider itself recorded them, not when we read
    /// them. `None` if the source carried no timestamp.
    pub observed_at: Option<i64>,
    /// Why the live request did not succeed. Useful in a bug report; the card
    /// itself only needs the age.
    pub reason: String,
}

/// What one provider card knows right now. Every provider fails independently:
/// a dead endpoint produces one `Error` card, never an empty dashboard.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ProviderQuota {
    Ok {
        provider: &'static str,
        plan: Option<String>,
        windows: Vec<QuotaWindow>,
        fetched_at: i64,
        /// `None` means the figures are live.
        stale: Option<Stale>,
        /// Empty unless the provider breaks a window down further.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        breakdown: Vec<UsageSlice>,
        /// Present when a failed live request fell back to these cached rows and
        /// the provider asked the scheduler to observe a cooldown.
        #[serde(skip_serializing_if = "Option::is_none")]
        retry_after_seconds: Option<u64>,
    },
    /// The provider answered, and the answer is that this account has no such
    /// quota. Distinct from `Error` because nothing is broken and retrying
    /// harder will never change it — only subscribing would. Most people who
    /// install the deck will have this on at least one card.
    Unavailable {
        provider: &'static str,
        message: String,
        fetched_at: i64,
    },
    /// No local sign-in, session record, or browser snapshot has ever been
    /// detected. The dashboard hides these providers from the main deck and
    /// offers them through onboarding instead of presenting five error cards
    /// to someone who only uses one service.
    #[serde(rename = "not_configured")]
    NotConfigured {
        provider: &'static str,
        message: String,
        fetched_at: i64,
    },
    /// The provider cannot recover without an explicit user action. This is
    /// not retried with error backoff: repeating the same request cannot renew
    /// a credential owned by another application.
    #[serde(rename = "action_required")]
    ActionRequired {
        provider: &'static str,
        message: String,
        fetched_at: i64,
    },
    Error {
        provider: &'static str,
        message: String,
        fetched_at: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        retry_after_seconds: Option<u64>,
    },
}

impl ProviderQuota {
    pub fn ok(provider: &'static str, plan: Option<String>, windows: Vec<QuotaWindow>) -> Self {
        Self::Ok {
            provider,
            plan,
            windows,
            fetched_at: now(),
            stale: None,
            breakdown: Vec::new(),
            retry_after_seconds: None,
        }
    }

    /// Attach a split of the window's usage. No-op on an error card.
    pub fn with_breakdown(mut self, slices: Vec<UsageSlice>) -> Self {
        if let Self::Ok { breakdown, .. } = &mut self {
            *breakdown = slices;
        }
        self
    }

    pub fn ok_stale(
        provider: &'static str,
        plan: Option<String>,
        windows: Vec<QuotaWindow>,
        stale: Stale,
    ) -> Self {
        Self::Ok {
            provider,
            plan,
            windows,
            fetched_at: now(),
            stale: Some(stale),
            breakdown: Vec::new(),
            retry_after_seconds: None,
        }
    }

    pub fn unavailable(provider: &'static str, message: impl Into<String>) -> Self {
        Self::Unavailable {
            provider,
            message: message.into(),
            fetched_at: now(),
        }
    }

    pub fn not_configured(provider: &'static str, message: impl Into<String>) -> Self {
        Self::NotConfigured {
            provider,
            message: message.into(),
            fetched_at: now(),
        }
    }

    pub fn action_required(provider: &'static str, message: impl Into<String>) -> Self {
        Self::ActionRequired {
            provider,
            message: message.into(),
            fetched_at: now(),
        }
    }

    pub fn error(provider: &'static str, message: impl Into<String>) -> Self {
        Self::Error {
            provider,
            message: message.into(),
            fetched_at: now(),
            retry_after_seconds: None,
        }
    }

    /// Preserve a provider-requested cooldown even when cached figures keep the
    /// card in the successful visual state.
    pub fn with_retry_after(mut self, seconds: Option<u64>) -> Self {
        match &mut self {
            Self::Ok {
                retry_after_seconds,
                ..
            }
            | Self::Error {
                retry_after_seconds,
                ..
            } => *retry_after_seconds = seconds,
            _ => {}
        }
        self
    }
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Replace a small state file without ever exposing truncated JSON to readers.
/// The staging file is process-specific so independent native-host processes
/// can update sibling caches without sharing temporary names.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let staging = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&staging, bytes)
        .map_err(|error| format!("cannot write {}: {error}", staging.display()))?;
    std::fs::rename(&staging, path).map_err(|error| {
        let _ = std::fs::remove_file(&staging);
        format!("cannot replace {}: {error}", path.display())
    })
}

/// Phrase a failing status so the card says what happened, not what the
/// protocol called it. A 429 in particular is the deck's own fault for asking
/// too often, and telling the user "429 Too Many Requests" invites them to
/// think their account is throttled.
pub fn http_failure(status: reqwest::StatusCode, subject: &str) -> String {
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        format!("{subject} is rate limiting these checks — the deck will slow down.")
    } else {
        format!("{subject} returned {status}")
    }
}

/// Parse an RFC 3339 timestamp into unix seconds. Providers disagree on the
/// wire format — Claude sends ISO strings, Codex sends unix integers — so
/// everything is normalised to seconds here.
pub fn rfc3339_to_unix(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
}

/// Name a window from its length.
///
/// Codex reports how long a window is instead of naming it, and which lengths
/// turn up depends on the plan: a free account's *first* slot is 30 days, so
/// "the first one is the 5-hour limit" is wrong in exactly the way that still
/// renders convincingly. Codex's own status line offers five-hour, daily,
/// weekly and monthly widgets, which is where this table comes from.
pub fn label_for_window_minutes(minutes: f64) -> String {
    const KNOWN: [(i64, &str); 4] = [
        (300, "Session (5h)"),
        (1440, "Daily"),
        (10080, "Weekly"),
        (43200, "Monthly"),
    ];

    let m = minutes.round() as i64;
    for (length, name) in KNOWN {
        // 2% tolerance absorbs rounding without letting two periods collide.
        if (m - length).abs() * 50 <= length {
            return name.to_string();
        }
    }

    // An unrecognised period is described as-is rather than forced into the
    // nearest bucket. An ugly correct label beats a tidy wrong one.
    if m % 1440 == 0 {
        format!("{}-day", m / 1440)
    } else if m % 60 == 0 {
        format!("{}-hour", m / 60)
    } else {
        format!("{m}-minute")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn names_the_four_windows_codex_reports() {
        assert_eq!(label_for_window_minutes(300.0), "Session (5h)");
        assert_eq!(label_for_window_minutes(1440.0), "Daily");
        assert_eq!(label_for_window_minutes(10080.0), "Weekly");
        assert_eq!(label_for_window_minutes(43200.0), "Monthly");
    }

    #[test]
    fn a_free_plans_first_slot_is_monthly_not_session() {
        // The whole reason this function exists. A free ChatGPT account reports
        // 2_592_000 seconds in `primary_window`; calling that "Session" because
        // it came first is the failure this guards against.
        assert_eq!(label_for_window_minutes(2_592_000.0 / 60.0), "Monthly");
    }

    #[test]
    fn tolerates_rounding_without_letting_periods_collide() {
        assert_eq!(label_for_window_minutes(299.4), "Session (5h)");
        assert_eq!(label_for_window_minutes(10_090.0), "Weekly");
        // Halfway between Weekly and Monthly must snap to neither.
        assert_eq!(label_for_window_minutes(26_640.0), "444-hour");
    }

    #[test]
    fn describes_an_unknown_period_rather_than_guessing() {
        assert_eq!(label_for_window_minutes(4320.0), "3-day");
        assert_eq!(label_for_window_minutes(120.0), "2-hour");
        assert_eq!(label_for_window_minutes(90.0), "90-minute");
    }

    #[test]
    fn not_configured_has_its_own_wire_status() {
        let json = serde_json::to_value(ProviderQuota::not_configured(
            "gemini",
            "Install the optional browser bridge.",
        ))
        .unwrap();
        assert_eq!(json["status"], "not_configured");
        assert_eq!(json["provider"], "gemini");
    }

    #[test]
    fn atomic_write_replaces_an_existing_state_file() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ai-quota-deck-atomic-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        std::fs::write(&path, b"old").unwrap();

        atomic_write(&path, b"new complete bytes").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new complete bytes");
        assert!(!path
            .with_extension(format!("tmp-{}", std::process::id()))
            .exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
