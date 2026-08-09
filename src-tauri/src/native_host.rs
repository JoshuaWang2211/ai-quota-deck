use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const HOST_NAME: &str = "me.joshuawang.ai_quota_deck";
pub const BRIDGE_ORIGIN: &str = "chrome-extension://alckoeangnmpomfnafaajjbpniomhnke/";
pub const GROK_ORIGIN: &str = "chrome-extension://bmpboaihdkpkjehbceegdmndkonlpdge/";
pub const GEMINI_ORIGIN: &str = "chrome-extension://cdepnbhodggenlkdeelnocfckdendhad/";
#[cfg(debug_assertions)]
pub const GROK_DEV_ORIGIN: &str = "chrome-extension://kmkhanodbnikodfikifdgclglbchmapb/";
#[cfg(debug_assertions)]
pub const GEMINI_DEV_ORIGIN: &str = "chrome-extension://gmclhkmckmlkcfdgifoplejaadejfhbb/";

const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Deserialize, Serialize)]
struct BrowserPush {
    version: u32,
    provider: String,
    observed_at: i64,
    payload: Value,
}

fn provider_for_origin(origin: &str) -> Option<&'static str> {
    match origin {
        GROK_ORIGIN => Some("grok"),
        #[cfg(debug_assertions)]
        GROK_DEV_ORIGIN => Some("grok"),
        GEMINI_ORIGIN => Some("gemini"),
        #[cfg(debug_assertions)]
        GEMINI_DEV_ORIGIN => Some("gemini"),
        _ => None,
    }
}

fn origin_allows_provider(origin: &str, provider: &str) -> bool {
    if origin == BRIDGE_ORIGIN {
        return matches!(provider, "grok" | "gemini");
    }
    provider_for_origin(origin) == Some(provider)
}

fn origin_is_allowed(origin: &str) -> bool {
    origin == BRIDGE_ORIGIN || provider_for_origin(origin).is_some()
}

fn allowed_origins() -> Vec<&'static str> {
    // Only the debug build pushes onto this, so release builds see an unused
    // `mut` rather than a mistake.
    #[allow(unused_mut)]
    let mut origins = vec![BRIDGE_ORIGIN, GROK_ORIGIN, GEMINI_ORIGIN];
    #[cfg(debug_assertions)]
    origins.push(GROK_DEV_ORIGIN);
    #[cfg(debug_assertions)]
    origins.push(GEMINI_DEV_ORIGIN);
    origins
}

fn cache_dir() -> Result<PathBuf, String> {
    dirs::data_local_dir()
        .map(|dir| dir.join("ai-quota-deck").join("browser-cache"))
        .ok_or_else(|| "could not locate the local application data directory".to_string())
}

fn cache_path(provider: &str) -> Result<PathBuf, String> {
    match provider {
        "grok" | "gemini" => Ok(cache_dir()?.join(format!("{provider}.json"))),
        _ => Err(format!("unsupported browser provider: {provider}")),
    }
}

pub fn read_cache(provider: &str) -> Result<String, String> {
    let path = cache_path(provider)?;
    fs::read_to_string(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

pub fn cache_exists(provider: &str) -> Result<bool, String> {
    let path = cache_path(provider)?;
    path.try_exists()
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))
}

/// How long a paid Grok snapshot stays worth protecting, matching the window the
/// dashboard will still display it for.
const PAID_SNAPSHOT_MAX_AGE_SECONDS: i64 = 24 * 60 * 60;

/// A signed-out grok.com tab still answers the free-tier endpoint, so the bridge
/// cannot tell "not signed in" from "signed in on the free plan" and pushes the
/// anonymous query allowance either way. Letting that land would replace a paid
/// account's real weekly figure with "2 queries left" for as long as the tab
/// stays open — and it re-pushes every three minutes. A free-only push is
/// therefore refused while a usable paid snapshot is still on disk.
///
/// Bounded on purpose: once the paid snapshot ages out or passes the reset it
/// reported, free data is allowed through, so a genuine downgrade still lands.
fn free_push_would_bury_paid(push: &BrowserPush, existing: &str) -> bool {
    if push.provider != "grok" || !push.payload["paid"].is_null() {
        return false;
    }
    let Ok(previous) = serde_json::from_str::<BrowserPush>(existing) else {
        return false;
    };
    if previous.payload["paid"].is_null() {
        return false;
    }
    if push.observed_at - previous.observed_at > PAID_SNAPSHOT_MAX_AGE_SECONDS {
        return false;
    }
    // `resetAt`, not `reset_at`: the bridge sends the provider's own camelCase
    // wire shape, which `BrowserPaid` only renames on the way into Rust.
    match previous.payload["paid"]["resetAt"].as_f64() {
        // Reported in milliseconds, as the dashboard reads it.
        Some(milliseconds) if milliseconds > 0.0 => {
            (milliseconds / 1000.0) as i64 > push.observed_at
        }
        _ => true,
    }
}

fn write_cache(push: &BrowserPush) -> Result<(), String> {
    let path = cache_path(&push.provider)?;
    if fs::read_to_string(&path).is_ok_and(|existing| free_push_would_bury_paid(push, &existing)) {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "browser cache path has no parent directory".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let bytes = serde_json::to_vec(push)
        .map_err(|error| format!("cannot serialize browser cache: {error}"))?;
    fs::write(&path, bytes).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn read_frame(reader: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut prefix = [0_u8; 4];
    let read = reader.read(&mut prefix[..1])?;
    if read == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut prefix[1..])?;

    let length = u32::from_ne_bytes(prefix) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("native message length {length} is outside 1..={MAX_FRAME_BYTES}"),
        ));
    }

    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

fn write_frame(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(value).map_err(io::Error::other)?;
    let length = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "native message is too large"))?;
    writer.write_all(&length.to_ne_bytes())?;
    writer.write_all(&body)?;
    writer.flush()
}

fn accept_message(origin: &str, body: &[u8]) -> Result<BrowserPush, String> {
    if !origin_is_allowed(origin) {
        return Err(format!("origin is not allowed: {origin}"));
    }
    let push: BrowserPush = serde_json::from_slice(body)
        .map_err(|error| format!("invalid native message JSON: {error}"))?;
    if push.version != 1 {
        return Err(format!(
            "unsupported browser message version: {}",
            push.version
        ));
    }
    if !origin_allows_provider(origin, &push.provider) {
        return Err(format!(
            "origin {origin} cannot write provider {}",
            push.provider
        ));
    }
    Ok(push)
}

pub fn run(origin: &str) -> Result<(), String> {
    if !origin_is_allowed(origin) {
        return Err(format!("origin is not allowed: {origin}"));
    }

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();

    loop {
        let Some(body) = read_frame(&mut reader).map_err(|error| error.to_string())? else {
            return Ok(());
        };

        let response = match accept_message(origin, &body).and_then(|push| {
            write_cache(&push)?;
            Ok(push)
        }) {
            Ok(push) => serde_json::json!({
                "ok": true,
                "provider": push.provider,
                "observed_at": push.observed_at
            }),
            Err(error) => serde_json::json!({ "ok": false, "error": error }),
        };
        write_frame(&mut writer, &response).map_err(|error| error.to_string())?;
    }
}

/// Chromium browsers each look for native messaging hosts under their own
/// `HKCU\Software\...` root, so registering Chrome alone reaches Chrome only.
/// Comet is the exception — it has no root of its own and reads Chrome's, which
/// is why it worked before this list existed.
const BROWSER_KEYS: [(&str, &str); 6] = [
    ("Chrome", r"Software\Google\Chrome"),
    ("Edge", r"Software\Microsoft\Edge"),
    ("Brave", r"Software\BraveSoftware\Brave-Browser"),
    ("Vivaldi", r"Software\Vivaldi"),
    ("Opera", r"Software\Opera Software\Opera Stable"),
    ("Chromium", r"Software\Chromium"),
];

#[cfg(windows)]
pub fn register() -> Result<PathBuf, String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let executable = std::env::current_exe()
        .map_err(|error| format!("cannot locate the running executable: {error}"))?;
    let directory = cache_dir()?.join("native-host");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
    let manifest_path = directory.join(format!("{HOST_NAME}.json"));
    let manifest = serde_json::json!({
        "name": HOST_NAME,
        "description": "AI Quota Deck browser quota bridge",
        "path": executable,
        "type": "stdio",
        "allowed_origins": allowed_origins()
    });
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("cannot serialize native host manifest: {error}"))?;
    fs::write(&manifest_path, bytes)
        .map_err(|error| format!("cannot write {}: {error}", manifest_path.display()))?;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let location = manifest_path.to_string_lossy().to_string();
    let mut registered = Vec::new();
    let mut failures = Vec::new();

    for (browser, root) in BROWSER_KEYS {
        // Only register browsers that are actually installed, rather than
        // leaving keys behind for every Chromium fork the user has never had.
        if hkcu.open_subkey(root).is_err() {
            continue;
        }
        let key_path = format!(r"{root}\NativeMessagingHosts\{HOST_NAME}");
        match hkcu
            .create_subkey(&key_path)
            .and_then(|(key, _)| key.set_value("", &location))
        {
            Ok(()) => registered.push(browser),
            Err(error) => failures.push(format!("{browser}: {error}")),
        }
    }

    if registered.is_empty() {
        return Err(format!(
            "no supported browser accepted the native host registration ({})",
            if failures.is_empty() {
                "none installed".to_string()
            } else {
                failures.join("; ")
            }
        ));
    }

    Ok(manifest_path)
}

#[cfg(not(windows))]
pub fn register() -> Result<PathBuf, String> {
    Err("native messaging registration is currently Windows-only".to_string())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn grok_message() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "provider": "grok",
            "observed_at": 1_800_000_000,
            "payload": { "paid": { "used": 12.5 } }
        }))
        .unwrap()
    }

    #[test]
    fn reads_and_writes_native_endian_length_prefixed_json() {
        let body = grok_message();
        let mut framed = Vec::new();
        framed.extend_from_slice(&(body.len() as u32).to_ne_bytes());
        framed.extend_from_slice(&body);

        let read = read_frame(&mut Cursor::new(framed)).unwrap().unwrap();
        assert_eq!(read, body);

        let mut response = Vec::new();
        write_frame(&mut response, &serde_json::json!({ "ok": true })).unwrap();
        let decoded = read_frame(&mut Cursor::new(response)).unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&decoded).unwrap(),
            serde_json::json!({ "ok": true })
        );
    }

    #[test]
    fn origin_can_only_write_its_own_provider() {
        assert!(accept_message(GROK_ORIGIN, &grok_message()).is_ok());
        #[cfg(debug_assertions)]
        assert!(accept_message(GROK_DEV_ORIGIN, &grok_message()).is_ok());

        let wrong_provider = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "provider": "gemini",
            "observed_at": 1_800_000_000,
            "payload": {}
        }))
        .unwrap();
        assert!(accept_message(GROK_ORIGIN, &wrong_provider).is_err());
        assert!(accept_message(BRIDGE_ORIGIN, &grok_message()).is_ok());
        assert!(accept_message(BRIDGE_ORIGIN, &wrong_provider).is_ok());
        assert!(accept_message("chrome-extension://not-allowed/", &grok_message()).is_err());
    }

    fn grok_push(observed_at: i64, paid: Value) -> BrowserPush {
        BrowserPush {
            version: 1,
            provider: "grok".to_string(),
            observed_at,
            payload: serde_json::json!({ "paid": paid, "buckets": [] }),
        }
    }

    /// Shaped like a real bridge push — `resetAt`, in milliseconds — so this
    /// fixture cannot agree with a guard that reads the wrong key.
    fn paid_snapshot(observed_at: i64, reset_at_ms: f64) -> String {
        serde_json::to_string(&grok_push(
            observed_at,
            serde_json::json!({ "used": 52.0, "resetAt": reset_at_ms }),
        ))
        .unwrap()
    }

    #[test]
    fn a_signed_out_tab_cannot_bury_a_paid_snapshot() {
        // Paid snapshot seen at t=1000, resetting well into the future.
        let existing = paid_snapshot(1_000, 9_000_000.0);
        let anonymous = grok_push(1_180, Value::Null);

        assert!(free_push_would_bury_paid(&anonymous, &existing));
    }

    /// Both payloads are the ones actually captured on 2026-08-09: the paid one
    /// from a signed-in Grok tab, the free one from the same machine with no
    /// session. Synthetic fixtures agree with whatever the guard reads, so the
    /// real wire shape is the only thing that catches a misspelled key.
    #[test]
    fn the_measured_anonymous_push_cannot_bury_the_measured_paid_snapshot() {
        let paid = r#"{"version":1,"provider":"grok","observed_at":1786267132,"payload":{
            "buckets":[],"unauthorized":false,"paid":{"used":54,"resetAt":1786360348000,
            "products":[{"id":4,"label":"Chat","percent":43}]}}}"#;
        let anonymous: BrowserPush = serde_json::from_str(
            r#"{"version":1,"provider":"grok","observed_at":1786267312,"payload":{
               "buckets":[{"key":"grok-3","label":"Fast","percent":100,"remaining":2,
               "total":2,"used":0}],"paid":null,"unauthorized":false}}"#,
        )
        .unwrap();

        assert!(free_push_would_bury_paid(&anonymous, paid));
    }

    #[test]
    fn a_paid_push_always_replaces_the_previous_one() {
        let existing = paid_snapshot(1_000, 9_000_000.0);
        let newer = grok_push(1_180, serde_json::json!({ "used": 61.0 }));

        assert!(!free_push_would_bury_paid(&newer, &existing));
    }

    #[test]
    fn free_data_lands_once_the_paid_snapshot_is_past_its_reset() {
        // Reset at 2000s; the push arrives after it, so the paid figure is spent.
        let existing = paid_snapshot(1_000, 2_000_000.0);
        let free = grok_push(2_500, Value::Null);

        assert!(!free_push_would_bury_paid(&free, &existing));
    }

    #[test]
    fn free_data_lands_once_the_paid_snapshot_is_a_day_old() {
        let existing = paid_snapshot(1_000, 9_000_000.0);
        let free = grok_push(1_000 + PAID_SNAPSHOT_MAX_AGE_SECONDS + 1, Value::Null);

        assert!(
            !free_push_would_bury_paid(&free, &existing),
            "a real downgrade must eventually be able to land"
        );
    }

    #[test]
    fn the_guard_is_grok_only_and_survives_a_corrupt_cache() {
        let existing = paid_snapshot(1_000, 9_000_000.0);
        let mut gemini = grok_push(1_180, Value::Null);
        gemini.provider = "gemini".to_string();

        assert!(!free_push_would_bury_paid(&gemini, &existing));
        assert!(!free_push_would_bury_paid(
            &grok_push(1_180, Value::Null),
            "not json"
        ));
    }

    #[test]
    fn registers_every_chromium_root_rather_than_chrome_alone() {
        let roots: Vec<&str> = BROWSER_KEYS.iter().map(|(_, root)| *root).collect();
        assert!(roots.contains(&r"Software\Google\Chrome"));
        assert!(roots.contains(&r"Software\Microsoft\Edge"));
        assert!(roots.contains(&r"Software\BraveSoftware\Brave-Browser"));
        // Comet reads Chrome's root and must not get one of its own.
        assert!(!roots.iter().any(|root| root.contains("Comet")));
        assert!(roots
            .iter()
            .all(|root| !root.ends_with("NativeMessagingHosts")));
    }

    #[test]
    fn release_origins_stay_separate_from_the_debug_extension() {
        assert!(allowed_origins().contains(&BRIDGE_ORIGIN));
        assert!(allowed_origins().contains(&GROK_ORIGIN));
        assert!(allowed_origins().contains(&GEMINI_ORIGIN));
        #[cfg(debug_assertions)]
        assert!(allowed_origins().contains(&GROK_DEV_ORIGIN));
        #[cfg(debug_assertions)]
        assert!(allowed_origins().contains(&GEMINI_DEV_ORIGIN));
    }

    #[test]
    fn rejects_oversized_frames_before_allocating_them() {
        let prefix = ((MAX_FRAME_BYTES + 1) as u32).to_ne_bytes();
        let error = read_frame(&mut Cursor::new(prefix)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
