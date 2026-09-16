//! Bounded, allowlisted diagnostics. Never persist raw headers or response bodies.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{path::Path, sync::Mutex};

const MAX_EVENTS: usize = 64;
const MAX_BODY_BYTES: usize = 16 * 1024;
static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default, Deserialize, Serialize)]
struct History {
    events: Vec<Value>,
}

fn append_to(path: &Path, event: Value) -> Result<(), String> {
    let mut history: History = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    history.events.push(event);
    if history.events.len() > MAX_EVENTS {
        history.events.drain(..history.events.len() - MAX_EVENTS);
    }
    let bytes = serde_json::to_vec_pretty(&history).map_err(|e| e.to_string())?;
    crate::quota::atomic_write(path, &bytes)
}

pub fn record(mut event: Value) {
    event["at"] = json!(crate::quota::now());
    event["app_version"] = json!(env!("CARGO_PKG_VERSION"));
    let Ok(_guard) = WRITE_LOCK.lock() else {
        return;
    };
    let Some(dir) = crate::claude::provider_cache_dir() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = append_to(&dir.join("claude-diagnostics.json"), event);
    }
}

fn error_kind(bytes: &[u8]) -> &'static str {
    let Ok(body) = serde_json::from_slice::<Value>(bytes) else {
        return "non_json";
    };
    match body.pointer("/error/type").and_then(Value::as_str) {
        Some("rate_limit_error") => "rate_limit_error",
        Some("authentication_error") => "authentication_error",
        Some("permission_error") => "permission_error",
        Some("overloaded_error") => "overloaded_error",
        Some("api_error") => "api_error",
        Some("invalid_request_error") => "invalid_request_error",
        _ => "unknown",
    }
}

pub async fn response(mut response: reqwest::Response) -> &'static str {
    let retry = response.headers().get(reqwest::header::RETRY_AFTER);
    let text = retry.and_then(|v| v.to_str().ok());
    let seconds = text.and_then(|v| v.parse::<u64>().ok());
    let date = text
        .and_then(|v| chrono::DateTime::parse_from_rfc2822(v).ok())
        .map(|v| v.timestamp());
    let mut event = json!({
        "event": "usage_response", "status": response.status().as_u16(),
        "retry_after_format": if retry.is_none() { "absent" } else if seconds.is_some() {
            "seconds"
        } else if date.is_some() { "http_date" } else { "unrecognized" },
        "retry_after_seconds": seconds, "retry_after_at": date,
    });
    let mut bytes = Vec::new();
    let kind = loop {
        match response.chunk().await {
            Ok(Some(chunk)) if bytes.len() + chunk.len() <= MAX_BODY_BYTES => bytes.extend(chunk),
            Ok(Some(_)) => break "body_too_large",
            Ok(None) => break error_kind(&bytes),
            Err(_) => break "body_read_failed",
        }
    };
    event["error_type"] = json!(kind);
    record(event);
    kind
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_fields_cannot_leak_arbitrary_server_text() {
        assert_eq!(error_kind(br#"{"error":{"type":"rate_limit_error","message":"secret-token"},"email":"private"}"#), "rate_limit_error");
        assert_eq!(
            error_kind(br#"{"error":{"type":"secret-token"}}"#),
            "unknown"
        );
        assert_eq!(error_kind(b"<html>private</html>"), "non_json");
    }

    #[test]
    fn history_is_bounded_and_replaces_existing_file() {
        let dir = std::env::temp_dir().join(format!("quota-diagnostics-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");
        std::fs::write(&path, b"invalid").unwrap();
        for i in 0..70 {
            append_to(&path, json!({"sequence": i})).unwrap();
        }
        let history: History = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(history.events.len(), MAX_EVENTS);
        assert_eq!(history.events[0]["sequence"], 6);
        assert_eq!(history.events[63]["sequence"], 69);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
