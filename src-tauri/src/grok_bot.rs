//! Grok Bot quota, read with the short-lived access token the signed-in desktop
//! app keeps in its Chromium profile. The refresh token is deliberately never
//! deserialised or decrypted: Grok Bot remains the sole owner of renewal.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};

use crate::claude::provider_cache_dir;
use crate::quota::{http_failure, now, ProviderQuota, QuotaWindow, Stale};

const PROVIDER: &str = "grok_bot";
const USAGE_URL: &str = "https://api2.cursor.sh/aiserver.v1.DashboardService/GetSandUsageStatus";
const REQUEST_TIMEOUT_SECONDS: u64 = 10;
const SNAPSHOT_MAX_AGE_SECONDS: i64 = 24 * 60 * 60;
const FALLBACK_CLIENT_VERSION: &str = "0.29.0";

static FETCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Deserialize)]
struct LocalState {
    os_crypt: OsCrypt,
}

#[derive(Deserialize)]
struct OsCrypt {
    encrypted_key: String,
}

#[derive(Deserialize)]
struct Secrets {
    #[serde(rename = "cursor-machine-id")]
    machine_id: String,
    #[serde(rename = "cursor-accounts")]
    accounts: String,
}

#[derive(Deserialize)]
struct Accounts {
    active: String,
    accounts: HashMap<String, Account>,
}

#[derive(Deserialize)]
struct Account {
    #[serde(rename = "cursor-access-token")]
    access_token: String,
    // Intentionally no refresh-token field. Unknown fields are discarded.
}

#[derive(Deserialize)]
struct JwtClaims {
    exp: Option<i64>,
}

struct Credentials {
    access_token: String,
    machine_id: String,
}

#[derive(Default, Debug, PartialEq)]
struct UsageStatus {
    current_period_start: Option<i64>,
    next_reset: Option<i64>,
    usage_percent: Option<f64>,
    included_limit_zero: bool,
    uses_pooled_enterprise_allowance: bool,
    has_available_usage: bool,
    has_non_zero_included_limit: bool,
    included_plan: Option<String>,
    plan_label: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct CachedUsage {
    plan: Option<String>,
    windows: Vec<QuotaWindow>,
    observed_at: i64,
}

fn profile_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("Grok Bot"))
}

fn snapshot_path() -> Option<PathBuf> {
    provider_cache_dir().map(|dir| dir.join("grok-bot.json"))
}

fn snapshot_exists() -> bool {
    snapshot_path().is_some_and(|path| matches!(path.try_exists(), Ok(true)))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path, subject: &str) -> Result<T, String> {
    let raw =
        std::fs::read_to_string(path).map_err(|error| format!("cannot read {subject}: {error}"))?;
    serde_json::from_str(&raw).map_err(|error| format!("cannot parse {subject}: {error}"))
}

#[cfg(target_os = "windows")]
fn dpapi_unprotect(bytes: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes
            .len()
            .try_into()
            .map_err(|_| "encrypted key is too large")?,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let succeeded = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if succeeded == 0 || output.pbData.is_null() {
        return Err(format!(
            "Windows could not unlock the Grok Bot profile ({})",
            std::io::Error::last_os_error()
        ));
    }
    let clear = unsafe {
        let value = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(output.pbData.cast());
        value
    };
    Ok(clear)
}

#[cfg(not(target_os = "windows"))]
fn dpapi_unprotect(_bytes: &[u8]) -> Result<Vec<u8>, String> {
    Err("Grok Bot profile decryption is supported on Windows only".to_string())
}

fn master_key(state: &LocalState) -> Result<Vec<u8>, String> {
    let wrapped = general_purpose::STANDARD
        .decode(&state.os_crypt.encrypted_key)
        .map_err(|error| format!("Grok Bot's encrypted key is not valid base64: {error}"))?;
    let payload = wrapped
        .strip_prefix(b"DPAPI")
        .ok_or("Grok Bot's encrypted key has an unknown format")?;
    let key = dpapi_unprotect(payload)?;
    if key.len() != 32 {
        return Err("Grok Bot's unlocked profile key has an unexpected length".to_string());
    }
    Ok(key)
}

fn decrypt_v10(encoded: &str, key: &[u8]) -> Result<Vec<u8>, String> {
    let blob = general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| format!("encrypted Grok Bot value is not valid base64: {error}"))?;
    if !blob.starts_with(b"v10") || blob.len() < 3 + 12 + 16 {
        return Err("encrypted Grok Bot value has an unknown format".to_string());
    }
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| "Grok Bot's profile key has an unexpected length".to_string())?;
    cipher
        .decrypt(Nonce::from_slice(&blob[3..15]), &blob[15..])
        .map_err(|_| "Grok Bot profile authentication failed".to_string())
}

fn decrypt_string(encoded: &str, key: &[u8], subject: &str) -> Result<String, String> {
    String::from_utf8(decrypt_v10(encoded, key)?)
        .map_err(|_| format!("decrypted {subject} is not text"))
}

fn jwt_exp(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let bytes = general_purpose::URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice::<JwtClaims>(&bytes).ok()?.exp
}

fn load_credentials_from(profile: &Path) -> Result<Credentials, String> {
    let state: LocalState = read_json(&profile.join("Local State"), "Grok Bot Local State")?;
    let secrets: Secrets = read_json(
        &profile.join("sand-secrets.json"),
        "Grok Bot's local sign-in record",
    )?;
    let key = master_key(&state)?;
    let accounts: Accounts = serde_json::from_str(&secrets.accounts)
        .map_err(|error| format!("cannot parse Grok Bot's account list: {error}"))?;
    let active = accounts
        .accounts
        .get(&accounts.active)
        .ok_or("Grok Bot's active account is missing")?;
    let access_token = decrypt_string(&active.access_token, &key, "Grok Bot access token")?;
    let machine_id = decrypt_string(&secrets.machine_id, &key, "Grok Bot machine id")?;
    if access_token.trim().is_empty() || machine_id.trim().is_empty() {
        return Err("Grok Bot's local sign-in record is incomplete".to_string());
    }
    Ok(Credentials {
        access_token,
        machine_id,
    })
}

fn checksum_at(machine_id: &str, epoch_millis: i64) -> String {
    // This reproduces Grok Bot's six JavaScript signed-shift bytes exactly.
    let bucket = epoch_millis.div_euclid(1_000_000) as u32;
    let mut bytes = [
        (bucket >> 8) as u8,
        bucket as u8,
        (bucket >> 24) as u8,
        (bucket >> 16) as u8,
        (bucket >> 8) as u8,
        bucket as u8,
    ];
    let mut state = 165_u8;
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = (*byte ^ state).wrapping_add(index as u8);
        state = *byte;
    }
    format!(
        "{}{}",
        general_purpose::URL_SAFE_NO_PAD.encode(bytes),
        machine_id
    )
}

fn read_varint(bytes: &[u8], cursor: &mut usize) -> Result<u64, String> {
    let mut value = 0_u64;
    for shift in (0..70).step_by(7) {
        let byte = *bytes.get(*cursor).ok_or("truncated protobuf varint")?;
        *cursor += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("protobuf varint is too long".to_string())
}

fn length_delimited<'a>(bytes: &'a [u8], cursor: &mut usize) -> Result<&'a [u8], String> {
    let length: usize = read_varint(bytes, cursor)?
        .try_into()
        .map_err(|_| "protobuf field is too large")?;
    let end = cursor
        .checked_add(length)
        .ok_or("protobuf field length overflow")?;
    let field = bytes.get(*cursor..end).ok_or("truncated protobuf field")?;
    *cursor = end;
    Ok(field)
}

fn skip_field(bytes: &[u8], cursor: &mut usize, wire: u64) -> Result<(), String> {
    match wire {
        0 => {
            read_varint(bytes, cursor)?;
        }
        1 => *cursor = cursor.checked_add(8).ok_or("protobuf field overflow")?,
        2 => {
            length_delimited(bytes, cursor)?;
        }
        5 => *cursor = cursor.checked_add(4).ok_or("protobuf field overflow")?,
        _ => return Err(format!("unsupported protobuf wire type {wire}")),
    }
    if *cursor > bytes.len() {
        return Err("truncated protobuf field".to_string());
    }
    Ok(())
}

fn timestamp_seconds(bytes: &[u8]) -> Result<i64, String> {
    let mut cursor = 0;
    let mut seconds = None;
    while cursor < bytes.len() {
        let key = read_varint(bytes, &mut cursor)?;
        let (field, wire) = (key >> 3, key & 7);
        if field == 1 && wire == 0 {
            seconds = Some(read_varint(bytes, &mut cursor)? as i64);
        } else {
            skip_field(bytes, &mut cursor, wire)?;
        }
    }
    seconds.ok_or("protobuf timestamp has no seconds".to_string())
}

fn optional_text(bytes: &[u8]) -> Option<String> {
    String::from_utf8(bytes.to_vec())
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn parse_usage_status(bytes: &[u8]) -> Result<UsageStatus, String> {
    let mut result = UsageStatus::default();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let key = read_varint(bytes, &mut cursor)?;
        let (field, wire) = (key >> 3, key & 7);
        match (field, wire) {
            (1, 2) => {
                result.current_period_start =
                    Some(timestamp_seconds(length_delimited(bytes, &mut cursor)?)?)
            }
            (2, 2) => {
                result.next_reset = Some(timestamp_seconds(length_delimited(bytes, &mut cursor)?)?)
            }
            (3, 1) => {
                let raw: [u8; 8] = bytes
                    .get(cursor..cursor + 8)
                    .ok_or("truncated usage percentage")?
                    .try_into()
                    .map_err(|_| "invalid usage percentage")?;
                result.usage_percent = Some(f64::from_le_bytes(raw));
                cursor += 8;
            }
            (4, 0) => result.included_limit_zero = read_varint(bytes, &mut cursor)? != 0,
            (6, 0) => {
                result.uses_pooled_enterprise_allowance = read_varint(bytes, &mut cursor)? != 0
            }
            (7, 0) => result.has_available_usage = read_varint(bytes, &mut cursor)? != 0,
            (8, 0) => result.has_non_zero_included_limit = read_varint(bytes, &mut cursor)? != 0,
            (14, 2) => result.included_plan = optional_text(length_delimited(bytes, &mut cursor)?),
            (15, 2) => result.plan_label = optional_text(length_delimited(bytes, &mut cursor)?),
            _ => skip_field(bytes, &mut cursor, wire)?,
        }
    }
    Ok(result)
}

fn humanize_plan(value: &str) -> String {
    value
        .split(['_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        })
        .collect::<Vec<String>>()
        .join(" ")
}

fn quota_from(status: UsageStatus) -> ProviderQuota {
    if status.uses_pooled_enterprise_allowance {
        return ProviderQuota::unavailable(
            PROVIDER,
            "Grok Bot uses a pooled enterprise allowance that does not report an individual percentage.",
        );
    }
    if status.included_limit_zero || !status.has_non_zero_included_limit {
        return ProviderQuota::unavailable(
            PROVIDER,
            "Signed in to Grok Bot, but this account reports no included usage limit.",
        );
    }
    let Some(percent) = status
        .usage_percent
        .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
    else {
        return ProviderQuota::unavailable(
            PROVIDER,
            "Grok Bot did not report an individual weekly usage percentage.",
        );
    };
    let window_seconds = status
        .current_period_start
        .zip(status.next_reset)
        .and_then(|(start, reset)| (reset > start).then_some(reset - start));
    let plan = status
        .plan_label
        .or_else(|| status.included_plan.as_deref().map(humanize_plan));
    ProviderQuota::ok(
        PROVIDER,
        plan,
        vec![QuotaWindow {
            label: "Weekly".to_string(),
            percent,
            resets_at: status.next_reset,
            window_seconds,
            severity: None,
        }],
    )
}

fn write_snapshot(plan: &Option<String>, windows: &[QuotaWindow]) -> Result<(), String> {
    let path = snapshot_path().ok_or("could not locate the local application data directory")?;
    let parent = path
        .parent()
        .ok_or("Grok Bot cache path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let bytes = serde_json::to_vec(&CachedUsage {
        plan: plan.clone(),
        windows: windows.to_vec(),
        observed_at: now(),
    })
    .map_err(|error| format!("cannot serialize Grok Bot cache: {error}"))?;
    let staging = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&staging, bytes)
        .map_err(|error| format!("cannot write {}: {error}", staging.display()))?;
    std::fs::rename(&staging, &path).map_err(|error| {
        let _ = std::fs::remove_file(&staging);
        format!("cannot replace {}: {error}", path.display())
    })
}

fn cached_quota_from_raw(raw: &str, current_time: i64, reason: &str) -> Option<ProviderQuota> {
    let mut snapshot: CachedUsage = serde_json::from_str(raw).ok()?;
    let age = current_time.checked_sub(snapshot.observed_at)?;
    if snapshot.observed_at <= 0 || !(0..=SNAPSHOT_MAX_AGE_SECONDS).contains(&age) {
        return None;
    }
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
            source: "last Grok Bot check".to_string(),
            observed_at: Some(snapshot.observed_at),
            reason: reason.to_string(),
        },
    ))
}

fn cached_quota(reason: &str) -> Option<ProviderQuota> {
    let raw = std::fs::read_to_string(snapshot_path()?).ok()?;
    cached_quota_from_raw(&raw, now(), reason)
}

fn fallback(reason: &str) -> ProviderQuota {
    cached_quota(reason).unwrap_or_else(|| ProviderQuota::error(PROVIDER, reason))
}

fn action_required(reason: &str) -> ProviderQuota {
    cached_quota(reason).unwrap_or_else(|| {
        ProviderQuota::action_required(
            PROVIDER,
            "Open Grok Bot and sign in again so it can renew its access token.",
        )
    })
}

pub async fn fetch() -> ProviderQuota {
    let _guard = FETCH_LOCK.lock().await;
    let Some(profile) = profile_dir() else {
        return ProviderQuota::not_configured(PROVIDER, "Install Grok Bot and sign in once.");
    };
    if !profile.join("sand-secrets.json").is_file() || !profile.join("Local State").is_file() {
        return if snapshot_exists() {
            action_required("Grok Bot's local sign-in record is missing.")
        } else {
            ProviderQuota::not_configured(PROVIDER, "Install Grok Bot and sign in once.")
        };
    }

    let credentials =
        match tauri::async_runtime::spawn_blocking(move || load_credentials_from(&profile)).await {
            Ok(Ok(credentials)) => credentials,
            Ok(Err(error)) => return action_required(&error),
            Err(error) => {
                return fallback(&format!(
                    "could not read Grok Bot's sign-in record: {error}"
                ))
            }
        };
    if jwt_exp(&credentials.access_token).is_some_and(|expiry| expiry <= now()) {
        return action_required("Grok Bot's access token has expired.");
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECONDS))
        .build()
    {
        Ok(client) => client,
        Err(error) => return fallback(&format!("could not create the Grok Bot client: {error}")),
    };
    let checksum = checksum_at(
        &credentials.machine_id,
        chrono::Utc::now().timestamp_millis(),
    );
    let response = match client
        .post(USAGE_URL)
        .header(
            "authorization",
            format!("Bearer {}", credentials.access_token),
        )
        .header("content-type", "application/proto")
        .header("connect-protocol-version", "1")
        .header("x-cursor-checksum", checksum)
        .header("x-cursor-client-type", "sand")
        .header("x-cursor-client-version", FALLBACK_CLIENT_VERSION)
        .header("x-sand-box-namespace", "prod")
        .header("x-ghost-mode", "true")
        .header("x-request-id", uuid_like_request_id())
        .body(Vec::new())
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => return fallback(&format!("Grok Bot usage request failed: {error}")),
    };
    if response.status() == reqwest::StatusCode::UNAUTHORIZED
        || response.status() == reqwest::StatusCode::FORBIDDEN
    {
        return action_required("Grok Bot rejected its saved access token.");
    }
    if !response.status().is_success() {
        return fallback(&http_failure(response.status(), "Grok Bot's usage service"));
    }
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            return fallback(&format!(
                "could not read Grok Bot's usage response: {error}"
            ))
        }
    };
    let status = match parse_usage_status(&bytes) {
        Ok(status) => status,
        Err(error) => {
            return fallback(&format!(
                "could not parse Grok Bot's usage response: {error}"
            ))
        }
    };
    let quota = quota_from(status);
    if let ProviderQuota::Ok { plan, windows, .. } = &quota {
        let _ = write_snapshot(plan, windows);
    }
    quota
}

fn uuid_like_request_id() -> String {
    // A request correlation id, not a credential. This avoids adding a UUID
    // dependency solely for a header the server treats as opaque.
    let now = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default() as u128;
    let pid = u128::from(std::process::id());
    let value = now ^ (pid << 64) ^ (&now as *const u128 as usize as u128);
    let hex = format!("{value:032x}");
    format!(
        "{}-{}-4{}-a{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(mut value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if value == 0 {
                return bytes;
            }
        }
    }

    fn field_varint(field: u64, value: u64) -> Vec<u8> {
        let mut bytes = varint(field << 3);
        bytes.extend(varint(value));
        bytes
    }

    fn field_bytes(field: u64, value: &[u8]) -> Vec<u8> {
        let mut bytes = varint((field << 3) | 2);
        bytes.extend(varint(value.len() as u64));
        bytes.extend(value);
        bytes
    }

    fn timestamp(value: i64) -> Vec<u8> {
        field_varint(1, value as u64)
    }

    #[test]
    fn decrypts_chromium_v10_aes_gcm_values() {
        let key = [7_u8; 32];
        let nonce = [9_u8; 12];
        let clear = b"short-lived-access-token";
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), clear.as_slice())
            .unwrap();
        let mut blob = b"v10".to_vec();
        blob.extend(nonce);
        blob.extend(ciphertext);
        assert_eq!(
            decrypt_v10(&general_purpose::STANDARD.encode(blob), &key).unwrap(),
            clear
        );
    }

    #[test]
    fn parses_the_weekly_usage_response_without_account_data() {
        let mut bytes = field_bytes(1, &timestamp(1_787_500_000));
        bytes.extend(field_bytes(2, &timestamp(1_788_104_800)));
        bytes.extend(varint((3 << 3) | 1));
        bytes.extend(5.933123_f64.to_le_bytes());
        bytes.extend(field_varint(7, 1));
        bytes.extend(field_varint(8, 1));
        bytes.extend(field_bytes(14, b"supergrok"));
        bytes.extend(field_bytes(15, b"SuperGrok"));
        // Unknown future length-delimited fields are ignored.
        bytes.extend(field_bytes(20, b"future"));

        let parsed = parse_usage_status(&bytes).unwrap();
        assert_eq!(parsed.current_period_start, Some(1_787_500_000));
        assert_eq!(parsed.next_reset, Some(1_788_104_800));
        assert_eq!(parsed.usage_percent, Some(5.933123));
        assert_eq!(parsed.plan_label.as_deref(), Some("SuperGrok"));
        assert!(parsed.has_non_zero_included_limit);

        let ProviderQuota::Ok {
            provider,
            plan,
            windows,
            ..
        } = quota_from(parsed)
        else {
            panic!("a metered Grok Bot account should produce a quota");
        };
        assert_eq!(provider, PROVIDER);
        assert_eq!(plan.as_deref(), Some("SuperGrok"));
        assert_eq!(windows[0].label, "Weekly");
        assert_eq!(windows[0].window_seconds, Some(604_800));
    }

    #[test]
    fn cache_never_outlives_a_day_or_a_reset() {
        let raw = serde_json::json!({
            "plan": "SuperGrok",
            "observed_at": 1_000,
            "windows": [{
                "label": "Weekly",
                "percent": 12.5,
                "resets_at": 200_000,
                "window_seconds": 604_800,
                "severity": null
            }]
        })
        .to_string();
        assert!(cached_quota_from_raw(&raw, 1_000 + SNAPSHOT_MAX_AGE_SECONDS, "offline").is_some());
        assert!(
            cached_quota_from_raw(&raw, 1_000 + SNAPSHOT_MAX_AGE_SECONDS + 1, "offline").is_none()
        );
        assert!(cached_quota_from_raw(&raw, 200_000, "offline").is_none());
    }

    #[test]
    fn checksum_is_stable_inside_one_server_bucket() {
        assert_eq!(
            checksum_at("machine", 1_800_000_000_000),
            checksum_at("machine", 1_800_000_999_999)
        );
        assert_ne!(
            checksum_at("machine", 1_800_000_000_000),
            checksum_at("machine", 1_800_001_000_000)
        );
        assert!(checksum_at("machine", 1_800_000_000_000).ends_with("machine"));
    }

    #[test]
    #[ignore = "manual integration check against the locally installed Grok Bot"]
    fn installed_grok_bot_returns_a_live_weekly_quota() {
        let ProviderQuota::Ok {
            provider,
            windows,
            stale: None,
            ..
        } = tauri::async_runtime::block_on(fetch())
        else {
            panic!("the installed Grok Bot did not return a live quota");
        };
        assert_eq!(provider, PROVIDER);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "Weekly");
    }
}
