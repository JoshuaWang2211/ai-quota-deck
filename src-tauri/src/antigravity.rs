//! Antigravity quota, read from the IDE's local language server over loopback.
//! The credential is a per-launch CSRF token on that server's command line, so
//! it is rediscovered on every poll and never written anywhere. See
//! ARCHITECTURE.md §7.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::claude::provider_cache_dir;
use crate::quota::{http_failure, now, rfc3339_to_unix, ProviderQuota, QuotaWindow, Stale};

const PROVIDER: &str = "antigravity";
const SERVICE_PATH: &str = "exa.language_server_pb.LanguageServerService";
/// The x64 and ARM builds of the language server differ only in name.
const LANGUAGE_SERVER_EXES: [&str; 2] = [
    "language_server_windows_x64.exe",
    "language_server_windows_arm.exe",
];
/// Each server opens a TLS listener, a plaintext twin and sometimes an LSP
/// socket. The wrong ones fail within milliseconds; anything past this count
/// is not a language server port.
const MAX_PORTS_PER_PROCESS: usize = 4;
const MAX_COMMAND_LINE_CHARS: usize = 32_768;
const REQUEST_TIMEOUT_SECONDS: u64 = 5;
/// Discovery plus a handful of loopback requests. Kept under Codex's 10 s so
/// this card never extends the dashboard's joint wait.
const FETCH_DEADLINE_SECONDS: u64 = 8;
/// The IDE's own state is what matters here: a snapshot older than a day says
/// "open the IDE" instead of pretending to be a reading.
const SNAPSHOT_MAX_AGE_SECONDS: i64 = 24 * 60 * 60;
/// Same policy as the Claude plan: it changes on upgrade and at no other time.
const PLAN_TTL_SECONDS: i64 = 6 * 3600;
const PLAN_RETRY_SECONDS: i64 = 30 * 60;
/// `GetUserStatus` is only asked for the plan name; this metadata is what the
/// IDE's own quota panel sends.
const USER_STATUS_BODY: &str =
    r#"{"metadata":{"ideName":"antigravity","extensionName":"antigravity","locale":"en"}}"#;

static PLAN_CACHE: Mutex<Option<CachedPlan>> = Mutex::new(None);

struct CachedPlan {
    plan: Option<String>,
    at: i64,
}

/// One running language server. `csrf` is the credential: it stays in memory,
/// never reaches a log line and never reaches the snapshot.
struct Candidate {
    pid: u32,
    csrf: String,
    enable_lsp: bool,
    ports: Vec<u16>,
}

struct ParsedCommandLine {
    csrf: String,
    enable_lsp: bool,
}

enum Failure {
    /// Connection, TLS or timeout — a wrong port or a server that just exited.
    Transport(String),
    /// The server answered but this build has no quota summary RPC.
    Unsupported,
    Http(reqwest::StatusCode),
    Shape(String),
}

impl Failure {
    fn message(&self) -> String {
        match self {
            Self::Transport(error) => {
                format!("request to the Antigravity language server failed: {error}")
            }
            Self::Unsupported => "the Antigravity language server has no quota summary".to_string(),
            Self::Http(status) => http_failure(*status, "the Antigravity language server"),
            Self::Shape(error) => format!("could not parse the Antigravity quota summary: {error}"),
        }
    }
}

#[derive(Deserialize)]
struct SummaryResponse {
    #[serde(default)]
    response: Option<Summary>,
}

#[derive(Deserialize, Default)]
struct Summary {
    #[serde(default)]
    groups: Vec<Group>,
}

#[derive(Deserialize)]
struct Group {
    #[serde(rename = "displayName", default)]
    display_name: String,
    #[serde(default)]
    buckets: Vec<Bucket>,
}

/// `remainingFraction` and `remainingAmount` are a oneof; only the fraction
/// has a denominator, so only it can become a percentage.
#[derive(Deserialize)]
struct Bucket {
    #[serde(default)]
    window: String,
    #[serde(rename = "remainingFraction", default)]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime", default)]
    reset_time: Option<String>,
    #[serde(default)]
    disabled: bool,
}

/// Only the two plan-name paths are declared, so serde never materialises the
/// `name` and `email` fields that ride along in the same response.
#[derive(Deserialize)]
struct UserStatusResponse {
    #[serde(rename = "userStatus", default)]
    user_status: Option<UserStatus>,
}

#[derive(Deserialize)]
struct UserStatus {
    #[serde(rename = "userTier", default)]
    user_tier: Option<UserTier>,
    #[serde(rename = "planStatus", default)]
    plan_status: Option<PlanStatus>,
}

#[derive(Deserialize)]
struct UserTier {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct PlanStatus {
    #[serde(rename = "planInfo", default)]
    plan_info: Option<PlanInfo>,
}

#[derive(Deserialize)]
struct PlanInfo {
    #[serde(rename = "planName", default)]
    plan_name: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct CachedUsage {
    plan: Option<String>,
    windows: Vec<QuotaWindow>,
    observed_at: i64,
}

// ── Process discovery ───────────────────────────────────────────────────────

fn is_language_server(exe_file: &[u16]) -> bool {
    let len = exe_file
        .iter()
        .position(|&char| char == 0)
        .unwrap_or(exe_file.len());
    let name = String::from_utf16_lossy(&exe_file[..len]);
    LANGUAGE_SERVER_EXES
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
}

/// `--flag value` or `--flag=value`. Whole-token comparison, so
/// `--extension_server_csrf_token` can never stand in for `--csrf_token`.
fn flag_value<'a>(tokens: &[&'a str], flag: &str) -> Option<&'a str> {
    tokens.iter().enumerate().find_map(|(index, token)| {
        if *token == flag {
            tokens.get(index + 1).copied()
        } else {
            token
                .strip_prefix(flag)
                .and_then(|rest| rest.strip_prefix('='))
        }
    })
}

/// Only an Antigravity language server counts — the same binary also ships
/// with other Codeium-based products, which carry a different `--app_data_dir`.
fn parse_cmdline(command_line: &str) -> Option<ParsedCommandLine> {
    let tokens: Vec<&str> = command_line.split_whitespace().collect();
    let app_data_dir = flag_value(&tokens, "--app_data_dir")?;
    if app_data_dir != "antigravity" && !app_data_dir.starts_with("antigravity-") {
        return None;
    }
    let csrf = flag_value(&tokens, "--csrf_token")?;
    if csrf.len() != 36
        || !csrf
            .chars()
            .all(|char| char.is_ascii_hexdigit() || char == '-')
    {
        return None;
    }
    Some(ParsedCommandLine {
        csrf: csrf.to_string(),
        enable_lsp: tokens
            .iter()
            .any(|token| *token == "--enable_lsp" || token.starts_with("--enable_lsp=")),
    })
}

/// The IDE-global server (no `--enable_lsp`) outlives workspace windows, so it
/// is asked first; pid order after that keeps the sequence stable between polls.
fn order_candidates(candidates: &mut [Candidate]) {
    candidates.sort_by_key(|candidate| (candidate.enable_lsp, candidate.pid));
}

#[cfg(windows)]
mod discovery {
    use std::ffi::c_void;
    use std::ptr;

    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PEB, PROCESS_BASIC_INFORMATION, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
        RTL_USER_PROCESS_PARAMETERS,
    };

    use super::{is_language_server, parse_cmdline, Candidate, MAX_COMMAND_LINE_CHARS};

    /// `GetExtendedTcpTable` takes the address family as a bare integer; this
    /// is AF_INET, the only family the language server listens on.
    const AF_INET: u32 = 2;

    /// The flag reports a language server whose command line could not be
    /// read at all, which is a different thing from no server being there:
    /// an elevated IDE refuses a reader that is not itself elevated.
    pub(super) fn discover() -> (Vec<Candidate>, bool) {
        let mut unreadable = false;
        let candidates = language_server_pids()
            .into_iter()
            .filter_map(|pid| {
                let Some(command_line) = command_line(pid) else {
                    unreadable = true;
                    return None;
                };
                let parsed = parse_cmdline(&command_line)?;
                Some(Candidate {
                    pid,
                    csrf: parsed.csrf,
                    enable_lsp: parsed.enable_lsp,
                    ports: listening_ports(pid),
                })
            })
            .collect();
        (candidates, unreadable)
    }

    fn language_server_pids() -> Vec<u32> {
        let mut pids = Vec::new();
        // SAFETY: the entry is sized as Toolhelp requires and the snapshot
        // handle is closed on every path out of the block.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return pids;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            if Process32FirstW(snapshot, &mut entry) != 0 {
                loop {
                    if is_language_server(&entry.szExeFile) {
                        pids.push(entry.th32ProcessID);
                    }
                    if Process32NextW(snapshot, &mut entry) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snapshot);
        }
        pids
    }

    /// The command line lives in the other process's PEB. Reading it needs
    /// only query and read rights, which the same user's processes grant; an
    /// elevated IDE refuses, and the card reports that rather than escalating.
    fn command_line(pid: u32) -> Option<String> {
        // SAFETY: the handle is checked before use and closed on every path.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
            if process.is_null() {
                return None;
            }
            let command_line = read_command_line(process);
            CloseHandle(process);
            command_line
        }
    }

    fn read_command_line(process: HANDLE) -> Option<String> {
        // SAFETY: every destination is a local of the size passed alongside
        // it, and every source address came from the process's own PEB chain.
        unsafe {
            let mut info: PROCESS_BASIC_INFORMATION = std::mem::zeroed();
            let mut written = 0u32;
            let status = NtQueryInformationProcess(
                process,
                ProcessBasicInformation,
                ptr::from_mut(&mut info).cast::<c_void>(),
                std::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32,
                &mut written,
            );
            if status != 0 || info.PebBaseAddress.is_null() {
                return None;
            }
            let mut peb: PEB = std::mem::zeroed();
            if ReadProcessMemory(
                process,
                info.PebBaseAddress.cast_const().cast::<c_void>(),
                ptr::from_mut(&mut peb).cast::<c_void>(),
                std::mem::size_of::<PEB>(),
                ptr::null_mut(),
            ) == 0
            {
                return None;
            }
            let mut params: RTL_USER_PROCESS_PARAMETERS = std::mem::zeroed();
            if ReadProcessMemory(
                process,
                peb.ProcessParameters.cast_const().cast::<c_void>(),
                ptr::from_mut(&mut params).cast::<c_void>(),
                std::mem::size_of::<RTL_USER_PROCESS_PARAMETERS>(),
                ptr::null_mut(),
            ) == 0
            {
                return None;
            }
            let chars = usize::from(params.CommandLine.Length) / 2;
            if chars == 0 || chars > MAX_COMMAND_LINE_CHARS {
                return None;
            }
            let mut buffer = vec![0u16; chars];
            if ReadProcessMemory(
                process,
                params.CommandLine.Buffer.cast_const().cast::<c_void>(),
                buffer.as_mut_ptr().cast::<c_void>(),
                chars * 2,
                ptr::null_mut(),
            ) == 0
            {
                return None;
            }
            Some(String::from_utf16_lossy(&buffer))
        }
    }

    /// Every IPv4 port this pid is listening on, ascending. The server picks
    /// its ports at launch and announces them nowhere else.
    fn listening_ports(pid: u32) -> Vec<u16> {
        let mut size = 0u32;
        // SAFETY: the first call only reports the size; the second writes into
        // a buffer of at least that many bytes, aligned for the u32 rows it
        // holds, and the row count comes from the table header it just wrote.
        let mut ports: Vec<u16> = unsafe {
            GetExtendedTcpTable(
                ptr::null_mut(),
                &mut size,
                0,
                AF_INET,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            );
            let mut buffer = vec![0u32; (size as usize).div_ceil(4) + 16];
            if GetExtendedTcpTable(
                buffer.as_mut_ptr().cast::<c_void>(),
                &mut size,
                0,
                AF_INET,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            ) != NO_ERROR
            {
                return Vec::new();
            }
            let table = &*buffer.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>();
            std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize)
                .iter()
                .filter(|row| row.dwOwningPid == pid)
                .map(|row| u16::from_be(row.dwLocalPort as u16))
                .collect()
        };
        ports.sort_unstable();
        ports.dedup();
        ports
    }
}

#[cfg(windows)]
use discovery::discover;

#[cfg(not(windows))]
fn discover() -> (Vec<Candidate>, bool) {
    (Vec::new(), false)
}

// ── Loopback requests ───────────────────────────────────────────────────────

/// Built only here. The server presents a self-signed certificate, so this
/// client trusts any certificate — acceptable solely because every URL it is
/// given is formatted from `127.0.0.1` below, no proxy can sit between, and a
/// redirect is refused rather than followed: the CSRF header must never be
/// replayed against a host this module did not choose.
fn local_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECONDS))
        .build()
        .map_err(|error| format!("could not build HTTP client: {error}"))
}

fn rpc_url(port: u16, rpc: &str) -> String {
    format!("https://127.0.0.1:{port}/{SERVICE_PATH}/{rpc}")
}

async fn call(
    client: &reqwest::Client,
    port: u16,
    csrf: &str,
    rpc: &str,
    body: &'static str,
) -> Result<reqwest::Response, Failure> {
    let response = client
        .post(rpc_url(port, rpc))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header("Connect-Protocol-Version", "1")
        .header("X-Codeium-Csrf-Token", csrf)
        .body(body)
        .send()
        .await
        .map_err(|error| Failure::Transport(error.to_string()))?;
    match response.status() {
        status if status.is_success() => Ok(response),
        reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::NOT_IMPLEMENTED => {
            Err(Failure::Unsupported)
        }
        status => Err(Failure::Http(status)),
    }
}

async fn summary(client: &reqwest::Client, port: u16, csrf: &str) -> Result<Summary, Failure> {
    let response = call(client, port, csrf, "RetrieveUserQuotaSummary", "{}").await?;
    let body: SummaryResponse = response
        .json()
        .await
        .map_err(|error| Failure::Shape(error.to_string()))?;
    Ok(body.response.unwrap_or_default())
}

fn plan_from(status: &UserStatusResponse) -> Option<String> {
    let user_status = status.user_status.as_ref()?;
    let tier = user_status
        .user_tier
        .as_ref()
        .and_then(|tier| tier.name.as_deref());
    let plan_name = user_status
        .plan_status
        .as_ref()
        .and_then(|plan| plan.plan_info.as_ref())
        .and_then(|info| info.plan_name.as_deref());
    tier.into_iter()
        .chain(plan_name)
        .map(str::trim)
        .find(|name| !name.is_empty())
        .map(str::to_string)
}

/// Never fails the card: the plan is a label on an otherwise complete reading.
async fn fetch_plan(client: &reqwest::Client, port: u16, csrf: &str) -> Option<String> {
    let response = call(client, port, csrf, "GetUserStatus", USER_STATUS_BODY)
        .await
        .ok()?;
    let status: UserStatusResponse = response.json().await.ok()?;
    plan_from(&status)
}

async fn plan(client: &reqwest::Client, port: u16, csrf: &str) -> Option<String> {
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

    let fetched = fetch_plan(client, port, csrf).await;
    if let Ok(mut guard) = PLAN_CACHE.lock() {
        *guard = Some(CachedPlan {
            plan: fetched.clone(),
            at: now(),
        });
    }
    fetched
}

// ── Mapping ─────────────────────────────────────────────────────────────────

/// `window` is the provider's own name for the period, like Claude's `kind`.
/// An unrecognised value is passed through rather than dropped.
fn label(window: &str, scope: &str) -> String {
    let base = match window {
        "5h" => "Session (5h)",
        "weekly" => "Weekly",
        other => other,
    };
    if scope.is_empty() {
        base.to_string()
    } else {
        format!("{base} · {scope}")
    }
}

fn window_seconds(window: &str) -> Option<i64> {
    match window {
        "5h" => Some(5 * 60 * 60),
        "weekly" => Some(7 * 24 * 60 * 60),
        _ => None,
    }
}

/// "Gemini Models" → "Gemini", "Claude and GPT models" → "Claude+GPT": short
/// enough for a Widget cell while still being the provider's own grouping.
fn group_short(display_name: &str) -> String {
    let name = display_name.trim();
    let stem = match name.len().checked_sub("models".len()) {
        Some(cut) if name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case("models") => {
            name[..cut].trim_end()
        }
        _ => name,
    };
    stem.replace(" and ", "+")
}

fn windows_from(summary: &Summary) -> Vec<QuotaWindow> {
    let mut windows = Vec::new();
    for group in &summary.groups {
        let scope = group_short(&group.display_name);
        for bucket in &group.buckets {
            if bucket.disabled {
                continue;
            }
            let Some(remaining) = bucket.remaining_fraction else {
                continue;
            };
            if !remaining.is_finite() || !(0.0..=1.0).contains(&remaining) {
                continue;
            }
            // At a full allowance `resetTime` is the server's last refresh plus
            // the window length — a placeholder that moves every few minutes,
            // not a deadline. Nothing has been spent, so nothing resets.
            let resets_at = if remaining >= 1.0 {
                None
            } else {
                bucket.reset_time.as_deref().and_then(rfc3339_to_unix)
            };
            windows.push(QuotaWindow {
                label: label(&bucket.window, &scope),
                percent: (1.0 - remaining) * 100.0,
                resets_at,
                window_seconds: window_seconds(&bucket.window),
                severity: None,
            });
        }
    }
    windows
}

/// The call worked and no bucket was usable: nothing is metered on this plan,
/// which is `unavailable`, not an error.
fn no_model_quota() -> ProviderQuota {
    ProviderQuota::unavailable(
        PROVIDER,
        "Signed in to Antigravity, but this account reports no model quota.",
    )
}

// ── Snapshot ────────────────────────────────────────────────────────────────

fn snapshot_path() -> Option<PathBuf> {
    provider_cache_dir().map(|dir| dir.join("antigravity.json"))
}

fn snapshot_exists() -> bool {
    snapshot_path().is_some_and(|path| matches!(path.try_exists(), Ok(true)))
}

fn write_snapshot(plan: &Option<String>, windows: &[QuotaWindow]) -> Result<(), String> {
    let path = snapshot_path().ok_or("could not locate the local application data directory")?;
    let parent = path
        .parent()
        .ok_or("Antigravity cache path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let snapshot = CachedUsage {
        plan: plan.clone(),
        windows: windows.to_vec(),
        observed_at: now(),
    };
    let bytes = serde_json::to_vec(&snapshot)
        .map_err(|error| format!("cannot serialize Antigravity cache: {error}"))?;
    // Write aside and rename so a reader sees either the old bytes or the new
    // ones, never a truncated file.
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

    // Never carry a spent session window across its reset.
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
            source: "last Antigravity IDE check".to_string(),
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

// ── Fetch ───────────────────────────────────────────────────────────────────

pub async fn fetch() -> ProviderQuota {
    match tokio::time::timeout(Duration::from_secs(FETCH_DEADLINE_SECONDS), try_fetch()).await {
        Ok(quota) => quota,
        Err(_) => fallback("the Antigravity IDE did not answer in time"),
    }
}

/// No language server is running, so there is nothing to ask. A snapshot the
/// deck wrote earlier is the only evidence this provider was ever in use.
fn idle_quota() -> ProviderQuota {
    if let Some(quota) = cached_quota("Antigravity IDE is not running.") {
        return quota;
    }
    if snapshot_exists() {
        ProviderQuota::action_required(PROVIDER, "Open Antigravity IDE to refresh its quota.")
    } else {
        ProviderQuota::not_configured(
            PROVIDER,
            "Install Google Antigravity IDE and sign in once to add Antigravity.",
        )
    }
}

async fn try_fetch() -> ProviderQuota {
    // Toolhelp and PEB reads are synchronous Win32 calls — off the runtime.
    let (mut candidates, unreadable) = tauri::async_runtime::spawn_blocking(discover)
        .await
        .unwrap_or_default();
    if candidates.is_empty() {
        // A server that is running but cannot be inspected is not an absent
        // one, and telling the user to install the IDE would be wrong.
        return if unreadable {
            fallback("cannot inspect the Antigravity language server; if the IDE runs as administrator, start it normally")
        } else {
            idle_quota()
        };
    }
    order_candidates(&mut candidates);

    let client = match local_client() {
        Ok(client) => client,
        Err(message) => return fallback(&message),
    };
    let mut last_failure = None;
    for candidate in &candidates {
        for &port in candidate.ports.iter().take(MAX_PORTS_PER_PROCESS) {
            match summary(&client, port, &candidate.csrf).await {
                Ok(summary) => return live_quota(&client, port, &candidate.csrf, &summary).await,
                Err(Failure::Unsupported) => {
                    return ProviderQuota::action_required(
                        PROVIDER,
                        "This version of AI Quota Deck needs a newer Antigravity IDE to read its quota summary.",
                    );
                }
                Err(failure) => last_failure = Some(failure),
            }
        }
    }
    let reason = last_failure.map_or_else(
        || "the Antigravity language server is not listening on any port".to_string(),
        |failure| failure.message(),
    );
    fallback(&reason)
}

async fn live_quota(
    client: &reqwest::Client,
    port: u16,
    csrf: &str,
    summary: &Summary,
) -> ProviderQuota {
    let windows = windows_from(summary);
    if windows.is_empty() {
        return no_model_quota();
    }
    // Only after the summary succeeded, and only for the label.
    let plan = plan(client, port, csrf).await;
    // Best-effort and quota-only. A cache write failure must never hide a live
    // reading, and neither the CSRF token nor the raw response reaches this file.
    let _ = write_snapshot(&plan, &windows);
    ProviderQuota::ok(PROVIDER, plan, windows)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Measured from a live IDE; carries no account identifier.
    const SUMMARY_FIXTURE: &str = r#"{"response":{"groups":[
 {"displayName":"Gemini Models","description":"Models within this group: Gemini Flash, Gemini Pro","buckets":[
  {"bucketId":"gemini-weekly","displayName":"Weekly Limit Remaining","description":"You have used some of your weekly limit, it will fully refresh in 2 days, 13 hours.","window":"weekly","remainingFraction":0.99684095,"resetTime":"2026-08-24T02:50:47Z"},
  {"bucketId":"gemini-5h","displayName":"Five Hour Limit Remaining","window":"5h","remainingFraction":1,"resetTime":"2026-08-21T18:06:34Z"}]},
 {"displayName":"Claude and GPT models","description":"Models within this group: Claude Opus, Claude Sonnet, GPT-OSS","buckets":[
  {"bucketId":"3p-weekly","displayName":"Weekly Limit Remaining","window":"weekly","remainingFraction":1,"resetTime":"2026-08-28T13:06:34Z"},
  {"bucketId":"3p-5h","displayName":"Five Hour Limit Remaining","window":"5h","remainingFraction":1,"resetTime":"2026-08-21T18:06:34Z"}]}],
 "description":"Within each group, models share a weekly limit and a 5-hour limit. Quota is consumed proportionally to the cost of the tokens."}}"#;

    const CSRF: &str = "11111111-2222-3333-4444-555555555555";
    const DECOY: &str = "99999999-8888-7777-6666-555555555555";

    fn summary_from(raw: &str) -> Summary {
        serde_json::from_str::<SummaryResponse>(raw)
            .unwrap()
            .response
            .unwrap_or_default()
    }

    fn candidate(pid: u32, enable_lsp: bool) -> Candidate {
        Candidate {
            pid,
            csrf: CSRF.to_string(),
            enable_lsp,
            ports: vec![],
        }
    }

    #[test]
    fn reads_the_csrf_token_in_space_and_equals_forms() {
        let spaced = format!(
            "\"C:\\Antigravity IDE\\language_server_windows_x64.exe\" --extension_server_csrf_token {DECOY} --csrf_token {CSRF} --app_data_dir antigravity"
        );
        let parsed = parse_cmdline(&spaced).unwrap();
        assert_eq!(parsed.csrf, CSRF);
        assert!(!parsed.enable_lsp);

        let equals = format!("ls.exe --app_data_dir=antigravity --csrf_token={CSRF}");
        assert_eq!(parse_cmdline(&equals).unwrap().csrf, CSRF);
    }

    #[test]
    fn rejects_a_server_that_is_not_antigravity() {
        assert!(parse_cmdline(&format!("ls.exe --csrf_token {CSRF}")).is_none());
        assert!(parse_cmdline(&format!(
            "ls.exe --app_data_dir windsurf --csrf_token {CSRF}"
        ))
        .is_none());
        assert!(parse_cmdline(&format!(
            "ls.exe --app_data_dir antigravityfoo --csrf_token {CSRF}"
        ))
        .is_none());
    }

    #[test]
    fn accepts_the_antigravity_ide_data_dir() {
        let parsed = parse_cmdline(&format!(
            "ls.exe --app_data_dir antigravity-ide --csrf_token {CSRF}"
        ))
        .unwrap();
        assert_eq!(parsed.csrf, CSRF);
    }

    #[test]
    fn never_takes_the_extension_server_token() {
        assert!(parse_cmdline(&format!(
            "ls.exe --app_data_dir antigravity-ide --extension_server_csrf_token {DECOY}"
        ))
        .is_none());
        assert!(parse_cmdline(&format!(
            "ls.exe --app_data_dir antigravity-ide --extension_server_csrf_token={DECOY}"
        ))
        .is_none());
        let both = format!(
            "ls.exe --extension_server_csrf_token={DECOY} --app_data_dir antigravity-ide --csrf_token {CSRF}"
        );
        assert_eq!(parse_cmdline(&both).unwrap().csrf, CSRF);
    }

    #[test]
    fn rejects_a_token_that_is_not_a_uuid() {
        assert!(parse_cmdline("ls.exe --app_data_dir antigravity --csrf_token short").is_none());
        assert!(parse_cmdline("ls.exe --app_data_dir antigravity --csrf_token").is_none());
    }

    #[test]
    fn records_the_lsp_flag() {
        let parsed = parse_cmdline(&format!(
            "ls.exe --csrf_token {CSRF} --enable_lsp --workspace_id w --app_data_dir antigravity-ide"
        ))
        .unwrap();
        assert!(parsed.enable_lsp);
    }

    #[test]
    fn prefers_the_server_without_lsp_then_lower_pid() {
        let mut candidates = vec![candidate(7616, true), candidate(25796, false)];
        order_candidates(&mut candidates);
        assert_eq!(candidates[0].pid, 25796);
        assert_eq!(candidates[1].pid, 7616);

        let mut same_kind = vec![candidate(300, false), candidate(200, false)];
        order_candidates(&mut same_kind);
        assert_eq!(same_kind[0].pid, 200);
    }

    #[test]
    fn matches_both_language_server_names_case_insensitively() {
        let utf16 = |name: &str| {
            let mut chars: Vec<u16> = name.encode_utf16().collect();
            chars.push(0);
            chars.resize(260, 0);
            chars
        };
        assert!(is_language_server(&utf16(
            "language_server_windows_x64.exe"
        )));
        assert!(is_language_server(&utf16(
            "LANGUAGE_SERVER_WINDOWS_ARM.EXE"
        )));
        assert!(!is_language_server(&utf16("language_server_linux_x64")));
        assert!(!is_language_server(&utf16("Antigravity IDE.exe")));
    }

    #[test]
    fn maps_the_measured_summary_onto_four_windows() {
        let windows = windows_from(&summary_from(SUMMARY_FIXTURE));
        assert_eq!(windows.len(), 4);

        let labels: Vec<&str> = windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Weekly · Gemini",
                "Session (5h) · Gemini",
                "Weekly · Claude+GPT",
                "Session (5h) · Claude+GPT"
            ]
        );

        assert!((windows[0].percent - 0.3159).abs() < 1e-4);
        assert_eq!(windows[1].percent, 0.0);
        assert_eq!(windows[2].percent, 0.0);
        assert_eq!(windows[3].percent, 0.0);

        assert_eq!(windows[0].window_seconds, Some(604_800));
        assert_eq!(windows[1].window_seconds, Some(18_000));
        assert_eq!(windows[2].window_seconds, Some(604_800));
        assert_eq!(windows[3].window_seconds, Some(18_000));

        // Only the bucket with something spent carries a real deadline; the
        // other three report a moving placeholder and get none.
        assert_eq!(windows[0].resets_at, Some(1_787_539_847));
        assert_eq!(
            windows[0].resets_at,
            rfc3339_to_unix("2026-08-24T02:50:47Z")
        );
        assert_eq!(windows[1].resets_at, None);
        assert_eq!(windows[2].resets_at, None);
        assert_eq!(windows[3].resets_at, None);

        assert!(windows.iter().all(|w| w.severity.is_none()));
    }

    #[test]
    fn passes_an_unknown_window_through_without_a_pace() {
        let raw = r#"{"response":{"groups":[{"displayName":"Gemini Models","buckets":[
            {"window":"daily","remainingFraction":0.5,"resetTime":"2026-08-22T00:00:00Z"}]}]}}"#;
        let windows = windows_from(&summary_from(raw));
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "daily · Gemini");
        assert_eq!(windows[0].percent, 50.0);
        assert_eq!(windows[0].window_seconds, None);
        assert_eq!(
            windows[0].resets_at,
            rfc3339_to_unix("2026-08-22T00:00:00Z")
        );
    }

    #[test]
    fn skips_disabled_amount_only_and_out_of_range_buckets() {
        let raw = r#"{"response":{"groups":[{"displayName":"Gemini Models","buckets":[
            {"window":"weekly","remainingFraction":0.5,"disabled":true},
            {"window":"weekly","remainingAmount":"12"},
            {"window":"5h","remainingFraction":1.5},
            {"window":"5h","remainingFraction":-0.1},
            {"window":"5h","remainingFraction":0.25}]}]}}"#;
        let windows = windows_from(&summary_from(raw));
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "Session (5h) · Gemini");
        assert_eq!(windows[0].percent, 75.0);
    }

    #[test]
    fn nothing_remaining_is_fully_used() {
        let raw = r#"{"response":{"groups":[{"displayName":"Claude and GPT models","buckets":[
            {"window":"weekly","remainingFraction":0,"resetTime":"2026-08-28T13:06:34Z"}]}]}}"#;
        let windows = windows_from(&summary_from(raw));
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "Weekly · Claude+GPT");
        assert_eq!(windows[0].percent, 100.0);
        assert_eq!(
            windows[0].resets_at,
            rfc3339_to_unix("2026-08-28T13:06:34Z")
        );
    }

    #[test]
    fn an_empty_summary_is_unavailable_not_an_error() {
        for raw in [
            r#"{"response":{"groups":[]}}"#,
            r#"{"response":{}}"#,
            r#"{}"#,
            r#"{"response":{"groups":[{"displayName":"Gemini Models","buckets":[
                {"window":"weekly","remainingFraction":0.5,"disabled":true}]}]}}"#,
        ] {
            assert!(windows_from(&summary_from(raw)).is_empty());
        }
        let ProviderQuota::Unavailable { provider, .. } = no_model_quota() else {
            panic!("a summary with no usable bucket must be unavailable");
        };
        assert_eq!(provider, PROVIDER);
    }

    #[test]
    fn shortens_group_names_without_inventing_them() {
        assert_eq!(group_short("Gemini Models"), "Gemini");
        assert_eq!(group_short("Claude and GPT models"), "Claude+GPT");
        assert_eq!(group_short("Imagen"), "Imagen");
        assert_eq!(label("weekly", ""), "Weekly");
    }

    #[test]
    fn snapshot_round_trips_and_drops_expired_rows() {
        // Observed eleven hours before the fixture's weekly deadline.
        let windows = windows_from(&summary_from(SUMMARY_FIXTURE));
        let raw = serde_json::to_string(&CachedUsage {
            plan: Some("Google AI Pro".to_string()),
            windows: windows.clone(),
            observed_at: 1_787_500_000,
        })
        .unwrap();
        assert!(!raw.contains(CSRF));

        let ProviderQuota::Ok {
            plan,
            windows: cached,
            stale: Some(stale),
            ..
        } = cached_quota_from_raw(&raw, 1_787_500_600, "Antigravity IDE is not running.").unwrap()
        else {
            panic!("a recent snapshot should stay visible as cached");
        };
        assert_eq!(plan.as_deref(), Some("Google AI Pro"));
        assert_eq!(cached.len(), 4);
        assert_eq!(cached[0].label, windows[0].label);
        assert_eq!(cached[0].percent, windows[0].percent);
        assert_eq!(cached[0].resets_at, windows[0].resets_at);
        assert_eq!(stale.observed_at, Some(1_787_500_000));
        assert_eq!(stale.source, "last Antigravity IDE check");

        // At the weekly deadline that row goes (the snapshot is still within a
        // day); the placeholder-free rows have no deadline and stay until the
        // snapshot itself ages out.
        let ProviderQuota::Ok { windows: later, .. } =
            cached_quota_from_raw(&raw, 1_787_539_847, "later").unwrap()
        else {
            panic!("rows without a deadline remain");
        };
        assert_eq!(later.len(), 3);
        assert!(later.iter().all(|w| w.resets_at.is_none()));
    }

    #[test]
    fn a_day_old_snapshot_is_not_presented_as_current() {
        let raw = serde_json::json!({
            "plan": "Google AI Pro",
            "observed_at": 1_000,
            "windows": [{
                "label": "Weekly · Gemini",
                "percent": 20.0,
                "resets_at": null,
                "window_seconds": 604_800,
                "severity": null
            }]
        })
        .to_string();
        assert!(cached_quota_from_raw(&raw, 1_000 + SNAPSHOT_MAX_AGE_SECONDS, "x").is_some());
        assert!(cached_quota_from_raw(&raw, 1_000 + SNAPSHOT_MAX_AGE_SECONDS + 1, "x").is_none());
        assert!(cached_quota_from_raw(&raw, 999, "x").is_none());
    }

    #[test]
    fn plan_prefers_the_tier_name_then_the_legacy_plan_name() {
        let both = r#"{"userStatus":{"name":"ignored","email":"ignored","userTier":{"id":"g1-pro-tier","name":"Google AI Pro"},"planStatus":{"planInfo":{"planName":"Pro"}}}}"#;
        let legacy = r#"{"userStatus":{"planStatus":{"planInfo":{"planName":"Pro"}}}}"#;
        let blank_tier = r#"{"userStatus":{"userTier":{"name":""},"planStatus":{"planInfo":{"planName":"Pro"}}}}"#;
        let neither = r#"{"userStatus":{"planStatus":{"availablePromptCredits":500}}}"#;
        let parse = |raw: &str| serde_json::from_str::<UserStatusResponse>(raw).unwrap();
        assert_eq!(plan_from(&parse(both)).as_deref(), Some("Google AI Pro"));
        assert_eq!(plan_from(&parse(legacy)).as_deref(), Some("Pro"));
        assert_eq!(plan_from(&parse(blank_tier)).as_deref(), Some("Pro"));
        assert_eq!(plan_from(&parse(neither)), None);
        assert_eq!(plan_from(&parse("{}")), None);
    }

    #[test]
    fn requests_target_loopback_only() {
        assert_eq!(
            rpc_url(51_887, "RetrieveUserQuotaSummary"),
            "https://127.0.0.1:51887/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary"
        );
    }

    /// Needs a signed-in Antigravity IDE running on this machine.
    #[test]
    #[ignore = "requires a running Antigravity IDE"]
    fn live_ide_reports_four_windows() {
        let quota = tauri::async_runtime::block_on(fetch());
        let ProviderQuota::Ok {
            plan,
            windows,
            stale,
            ..
        } = &quota
        else {
            panic!(
                "expected a live reading, got {}",
                serde_json::to_string(&quota).unwrap()
            );
        };
        assert!(stale.is_none(), "a running IDE must answer live");
        assert_eq!(windows.len(), 4);
        for window in windows {
            assert!(
                window.label.starts_with("Weekly") || window.label.starts_with("Session (5h)"),
                "unexpected label {}",
                window.label
            );
        }
        println!("status=ok plan={plan:?}");
        for window in windows {
            println!(
                "  {} percent={:.4} window_seconds={:?} resets_at={:?}",
                window.label, window.percent, window.window_seconds, window.resets_at
            );
        }
    }
}
