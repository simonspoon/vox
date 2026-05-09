//! Aspect chat — direct Anthropic Messages API integration (S03).
//!
//! Closes devil findings:
//! - M4 — system prompt re-loaded from `simaris show <slug> --content` per turn
//!   (never cached, never baked once); aspect mutations propagate immediately.
//! - A1 — system prompt is a plain Rust `String`; no shell quoting hazards.
//! - A2 — Anthropic Messages is a stateless API; no server-side session TTL
//!   to worry about.
//! - FM3 — non-2xx, network errors, missing key, malformed body are all returned
//!   as `Err`; the listen loop converts them to `[<slug>] error: <reason>` replies
//!   without panicking or halting polling.
//! - FM5 — the API call itself runs in a `std::thread::spawn` from listen.rs;
//!   delivery back to the main thread is via `mpsc::channel` (see
//!   `AspectReplyResult`), keeping the Telegram getUpdates loop unblocked.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::time::Duration;

use crate::session::ChatMessage;

const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
const MAX_TOKENS: u32 = 4096;

/// One completed (or failed) aspect-chat turn delivered from a worker thread
/// back to the listen loop. The main loop is responsible for editing the
/// `thinking…` Telegram message, sending chunks, and persisting the turn into
/// `SessionStore` on success.
#[derive(Debug)]
pub struct AspectReplyResult {
    pub chat_id: i64,
    pub thinking_msg_id: i64,
    pub slug: String,
    pub user_text: String,
    /// Reply text on success, or a short human-readable error reason on failure.
    pub result: Result<String, String>,
    /// M1 (code review 019e0eb7-06b9): generation captured at dispatch time.
    /// On delivery, listen.rs compares this to the current
    /// `SessionStore::generation(chat_id, slug)` and drops the turn if they
    /// differ — preventing a `/reset` issued during the in-flight call from
    /// being silently undone by the late `append_turn`.
    pub generation: u64,
}

#[derive(Debug, Serialize)]
struct ApiMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Serialize)]
struct ApiRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<ApiMessage<'a>>,
}

#[derive(Debug, Deserialize)]
struct ApiContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    #[serde(default)]
    content: Vec<ApiContentBlock>,
}

/// Re-load aspect system prompt from `simaris show <slug> --content`.
/// Devil-M4: this is called fresh on every turn — never cached.
pub fn load_system_prompt(slug: &str) -> Result<String> {
    let output = Command::new("simaris")
        .args(["show", slug, "--content"])
        .output()
        .context("failed to spawn `simaris`")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("simaris show {slug} failed: {}", stderr.trim());
    }
    let body = String::from_utf8(output.stdout).context("simaris stdout not UTF-8")?;
    Ok(body.trim().to_string())
}

/// Validate that `slug` exists in simaris (used by `/new <slug>` in S06).
pub fn aspect_exists(slug: &str) -> bool {
    Command::new("simaris")
        .args(["show", slug])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Call the Anthropic Messages API synchronously.
///
/// `history` MUST already include the latest user turn at the end. Caller
/// (listen.rs) is responsible for snapshot semantics — this function never
/// touches the on-disk session store.
pub fn call_anthropic(api_key: &str, system: &str, history: &[ChatMessage]) -> Result<String> {
    if api_key.is_empty() {
        bail!("missing Anthropic API key");
    }
    if history.is_empty() {
        bail!("empty message history");
    }
    let messages: Vec<ApiMessage> = history
        .iter()
        .map(|m| ApiMessage {
            role: m.role.as_str(),
            content: m.content.as_str(),
        })
        .collect();
    let body = ApiRequest {
        model: DEFAULT_MODEL,
        max_tokens: MAX_TOKENS,
        system,
        messages,
    };
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .context("build http client")?;
    let resp = client
        .post(ANTHROPIC_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .context("anthropic request failed")?;
    let status = resp.status();
    if !status.is_success() {
        let snippet: String = resp.text().unwrap_or_default().chars().take(200).collect();
        bail!("anthropic {}: {}", status, snippet);
    }
    let api_resp: ApiResponse = resp.json().context("parse anthropic response")?;
    let text = api_resp
        .content
        .iter()
        .filter(|b| b.block_type == "text")
        .filter_map(|b| b.text.as_deref())
        .collect::<Vec<_>>()
        .join("");
    if text.is_empty() {
        bail!("anthropic returned empty content");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    // S03-AC2: history is serialised as the messages array.
    #[test]
    fn api_request_serialises_messages_array() {
        let msgs = vec![
            ApiMessage {
                role: "user",
                content: "hi",
            },
            ApiMessage {
                role: "assistant",
                content: "hello",
            },
            ApiMessage {
                role: "user",
                content: "what color did I say?",
            },
        ];
        let req = ApiRequest {
            model: "test-model",
            max_tokens: 100,
            system: "you are lotus",
            messages: msgs,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "test-model");
        assert_eq!(json["system"], "you are lotus");
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][1]["role"], "assistant");
        assert_eq!(json["messages"][2]["content"], "what color did I say?");
    }

    // S03-FM3: API response parses content blocks correctly.
    #[test]
    fn api_response_extracts_text_blocks() {
        let json = r#"{
            "content": [
                {"type": "text", "text": "hello "},
                {"type": "text", "text": "world"}
            ]
        }"#;
        let r: ApiResponse = serde_json::from_str(json).unwrap();
        let combined: String = r
            .content
            .iter()
            .filter(|b| b.block_type == "text")
            .filter_map(|b| b.text.as_deref())
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(combined, "hello world");
    }

    // S03-FM3: empty key path returns Err, no panic.
    #[test]
    fn empty_key_returns_err() {
        let history = vec![ChatMessage {
            role: "user".into(),
            content: "hi".into(),
        }];
        let r = call_anthropic("", "system", &history);
        assert!(r.is_err());
    }

    // S03-FM3: empty history returns Err, no panic.
    #[test]
    fn empty_history_returns_err() {
        let r = call_anthropic("sk-ant-test", "system", &[]);
        assert!(r.is_err());
    }
}
