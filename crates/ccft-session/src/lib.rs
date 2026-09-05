//! Session-id extraction from hyper headers + JSON metadata bodies.
//!
//! Ported from cc-flytrap.py `extract_session_id()`. Order of preference, all
//! observed in real Claude Code / OpenAI / SDK traffic:
//!   1. `X-Claude-Code-Session-Id` header (Claude Code, every request)
//!   2. Other Anthropic/SDK session-ish headers (defensive)
//!   3. `metadata.session_id` (apps using the SDK metadata field directly)
//!   4. `metadata.user_id` parsed as JSON — Claude Code wraps device,
//!      account, and session UUIDs in a single JSON-encoded user_id
//!   5. SHA-1 of `metadata.user_id` — stable per-user bucket fallback

use hyper::HeaderMap;
use serde_json::Value;
use sha1::{Digest, Sha1};

const HEADER_PRIMARY: &str = "x-claude-code-session-id";
const HEADER_FALLBACKS: &[&str] = &[
    "anthropic-session-id",
    "x-session-id",
    "x-anthropic-session-id",
    "anthropic-conversation-id",
    "x-anthropic-conversation-id",
];

/// Extract a session id from request headers (and, optionally, the body).
pub fn extract(headers: &HeaderMap, body_bytes: Option<&[u8]>) -> Option<String> {
    // 1. Primary header
    if let Some(s) = header_str(headers, HEADER_PRIMARY) {
        return Some(s);
    }

    // 2. Defensive fallbacks
    for h in HEADER_FALLBACKS {
        if let Some(s) = header_str(headers, h) {
            return Some(s);
        }
    }

    // 3-5: parse the body's metadata field
    let body = body_bytes?;
    let data: Value = serde_json::from_slice(body).ok()?;
    let meta = data.get("metadata")?.as_object()?;

    // 3. metadata.session_id / sessionId
    if let Some(s) = meta.get("session_id").and_then(Value::as_str) {
        return Some(s.to_string());
    }
    if let Some(s) = meta.get("sessionId").and_then(Value::as_str) {
        return Some(s.to_string());
    }

    // 4. metadata.user_id as JSON-encoded blob
    let uid = meta.get("user_id").and_then(Value::as_str)?;
    if uid.starts_with('{') {
        if let Ok(blob) = serde_json::from_str::<Value>(uid) {
            if let Some(s) = blob.get("session_id").and_then(Value::as_str) {
                return Some(s.to_string());
            }
            if let Some(s) = blob.get("sessionId").and_then(Value::as_str) {
                return Some(s.to_string());
            }
        }
    }

    // Legacy `..._session_<uuid>` pattern (kept for older SDKs).
    if let Some(idx) = uid.find("_session_") {
        let after = &uid[idx + "_session_".len()..];
        let mut chars = after.chars();
        if let Some(first) = chars.next() {
            if first.is_ascii_hexdigit() {
                let take_n = after
                    .chars()
                    .take_while(|c| c.is_ascii_hexdigit() || *c == '-')
                    .count();
                if take_n >= 7 {
                    return Some(after[..take_n].to_string());
                }
            }
        }
    }

    // 5. SHA-1 fallback for stable per-user bucket
    if !uid.is_empty() {
        let mut hasher = Sha1::new();
        hasher.update(uid.as_bytes());
        let digest = hasher.finalize();
        let hex = hex::encode(&digest[..8]); // 16 hex chars
        return Some(format!("u-{hex}"));
    }

    None
}

fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name)?.to_str().ok().map(str::to_string)
}
