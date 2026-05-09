use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

/// Telegram Bot API client.
#[derive(Clone)]
pub struct TelegramClient {
    token: String,
    client: reqwest::blocking::Client,
}

// --- Telegram API types ---

#[derive(Debug, Deserialize)]
pub struct TelegramResponse<T> {
    pub ok: bool,
    pub result: Option<T>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct User {
    pub id: i64,
    #[serde(default)]
    pub is_bot: bool,
    pub first_name: String,
    pub last_name: Option<String>,
    pub username: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub username: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Document {
    pub file_id: String,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PhotoSize {
    pub file_id: String,
    pub width: i64,
    pub height: i64,
    pub file_size: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramFile {
    #[allow(dead_code)]
    pub file_id: String,
    pub file_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    pub message_id: i64,
    pub from: Option<User>,
    pub chat: Chat,
    pub text: Option<String>,
    pub reply_to_message: Option<Box<Message>>,
    pub document: Option<Document>,
    pub photo: Option<Vec<PhotoSize>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    pub message: Option<Message>,
    pub data: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
    pub callback_query: Option<CallbackQuery>,
}

#[derive(Debug, Serialize)]
struct SendMessageRequest<'a> {
    chat_id: &'a str,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_mode: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    disable_notification: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to_message_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct InlineKeyboardButton {
    pub text: String,
    pub callback_data: String,
}

#[derive(Debug, Serialize)]
pub struct InlineKeyboardMarkup {
    pub inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
}

#[derive(Debug, Serialize)]
struct SendMessageWithKeyboardRequest<'a> {
    chat_id: &'a str,
    text: &'a str,
    parse_mode: &'a str,
    reply_markup: InlineKeyboardMarkup,
    #[serde(skip_serializing_if = "Option::is_none")]
    disable_notification: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to_message_id: Option<i64>,
}

#[derive(Debug, Serialize)]
struct EditMessageTextRequest<'a> {
    chat_id: &'a str,
    message_id: i64,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_mode: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct AnswerCallbackQueryRequest<'a> {
    callback_query_id: &'a str,
}

impl TelegramClient {
    pub fn new(token: String) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("Failed to build HTTP client");
        Self { token, client }
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{method}", self.token)
    }

    /// Send a message to a chat using legacy MarkdownV1 parse mode. Returns
    /// the sent Message. Used for `vox notify` and other legacy-formatted
    /// messages where Markdown rendering is intentional.
    pub fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        silent: bool,
        reply_to: Option<i64>,
    ) -> Result<Message> {
        self.send_inner(chat_id, text, silent, reply_to, Some("Markdown"))
    }

    /// Send a message as plain text — no `parse_mode`, so unbalanced `_`, `*`,
    /// backticks, or `[`/`]` characters never trigger Telegram's "can't parse
    /// entities" 400. Used for aspect-chat replies, the `<slug> thinking…`
    /// interstitial, and any system-command reply that contains `[<slug>]`
    /// brackets (which MarkdownV1 treats as link openers). C1 (code review
    /// 019e0eb7-06b9, 2026-05-09).
    pub fn send_text(
        &self,
        chat_id: &str,
        text: &str,
        silent: bool,
        reply_to: Option<i64>,
    ) -> Result<Message> {
        self.send_inner(chat_id, text, silent, reply_to, None)
    }

    fn send_inner(
        &self,
        chat_id: &str,
        text: &str,
        silent: bool,
        reply_to: Option<i64>,
        parse_mode: Option<&str>,
    ) -> Result<Message> {
        let body = SendMessageRequest {
            chat_id,
            text,
            parse_mode,
            disable_notification: if silent { Some(true) } else { None },
            reply_to_message_id: reply_to,
        };

        let resp: TelegramResponse<Message> = self
            .client
            .post(self.api_url("sendMessage"))
            .json(&body)
            .send()
            .context("Failed to send message to Telegram")?
            .json()
            .context("Failed to parse Telegram sendMessage response")?;

        if !resp.ok {
            bail!(
                "Telegram sendMessage failed: {}",
                resp.description.unwrap_or_default()
            );
        }

        resp.result.context("Telegram returned ok but no result")
    }

    /// Poll for updates using long polling.
    pub fn get_updates(&self, offset: Option<i64>, timeout: u64) -> Result<Vec<Update>> {
        let mut url = self.api_url("getUpdates");
        let mut params = vec![format!("timeout={timeout}")];
        params.push("allowed_updates=[\"message\",\"callback_query\"]".to_string());
        if let Some(off) = offset {
            params.push(format!("offset={off}"));
        }
        url = format!("{url}?{}", params.join("&"));

        let resp: TelegramResponse<Vec<Update>> = self
            .client
            .get(&url)
            .send()
            .context("Failed to get updates from Telegram")?
            .json()
            .context("Failed to parse Telegram getUpdates response")?;

        if !resp.ok {
            bail!(
                "Telegram getUpdates failed: {}",
                resp.description.unwrap_or_default()
            );
        }

        Ok(resp.result.unwrap_or_default())
    }

    /// Best-effort commit of the acknowledged offset. Errors are swallowed.
    ///
    /// Telegram only marks updates as confirmed when `get_updates` is called
    /// with an offset greater than the update_id. Poll commands advance their
    /// local offset as they consume updates, but without this call the server
    /// never learns the advance and the next invocation replays the same
    /// updates.
    pub fn commit_offset(&self, offset: i64) {
        let _ = self.get_updates(Some(offset), 0);
    }

    /// Send a message with an inline keyboard to a chat. Returns the sent Message.
    pub fn send_message_with_keyboard(
        &self,
        chat_id: &str,
        text: &str,
        reply_markup: InlineKeyboardMarkup,
        silent: bool,
        reply_to: Option<i64>,
    ) -> Result<Message> {
        let body = SendMessageWithKeyboardRequest {
            chat_id,
            text,
            parse_mode: "Markdown",
            reply_markup,
            disable_notification: if silent { Some(true) } else { None },
            reply_to_message_id: reply_to,
        };

        let resp: TelegramResponse<Message> = self
            .client
            .post(self.api_url("sendMessage"))
            .json(&body)
            .send()
            .context("Failed to send message to Telegram")?
            .json()
            .context("Failed to parse Telegram sendMessage response")?;

        if !resp.ok {
            bail!(
                "Telegram sendMessage failed: {}",
                resp.description.unwrap_or_default()
            );
        }

        resp.result.context("Telegram returned ok but no result")
    }

    /// Acknowledge a callback query (dismiss the "loading" state on the button).
    pub fn answer_callback_query(&self, callback_query_id: &str) -> Result<()> {
        let body = AnswerCallbackQueryRequest { callback_query_id };

        let resp: TelegramResponse<bool> = self
            .client
            .post(self.api_url("answerCallbackQuery"))
            .json(&body)
            .send()
            .context("Failed to answer callback query")?
            .json()
            .context("Failed to parse Telegram answerCallbackQuery response")?;

        if !resp.ok {
            bail!(
                "Telegram answerCallbackQuery failed: {}",
                resp.description.unwrap_or_default()
            );
        }

        Ok(())
    }

    /// Edit a message's text in-place using legacy MarkdownV1. Returns the
    /// edited Message.
    pub fn edit_message_text(&self, chat_id: &str, message_id: i64, text: &str) -> Result<Message> {
        self.edit_inner(chat_id, message_id, text, Some("Markdown"))
    }

    /// Edit a message's text as plain text — no `parse_mode`. C1 fix: aspect
    /// reply chunks and `<slug> thinking…` interstitials must never trigger
    /// MarkdownV1 parse errors on unbalanced `_`, `*`, backticks, or `[`/`]`.
    pub fn edit_text(&self, chat_id: &str, message_id: i64, text: &str) -> Result<Message> {
        self.edit_inner(chat_id, message_id, text, None)
    }

    fn edit_inner(
        &self,
        chat_id: &str,
        message_id: i64,
        text: &str,
        parse_mode: Option<&str>,
    ) -> Result<Message> {
        let body = EditMessageTextRequest {
            chat_id,
            message_id,
            text,
            parse_mode,
        };

        let resp: TelegramResponse<Message> = self
            .client
            .post(self.api_url("editMessageText"))
            .json(&body)
            .send()
            .context("Failed to edit message on Telegram")?
            .json()
            .context("Failed to parse Telegram editMessageText response")?;

        if !resp.ok {
            bail!(
                "Telegram editMessageText failed: {}",
                resp.description.unwrap_or_default()
            );
        }

        resp.result.context("Telegram returned ok but no result")
    }

    /// Send a file (document) to a chat. Returns the sent Message.
    pub fn send_document(
        &self,
        chat_id: &str,
        file_path: &Path,
        caption: Option<&str>,
        silent: bool,
        reply_to: Option<i64>,
    ) -> Result<Message> {
        let form = reqwest::blocking::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .file("document", file_path)
            .context("Failed to read file for upload")?;

        let form = if let Some(cap) = caption {
            form.text("caption", cap.to_string())
                .text("parse_mode", "Markdown".to_string())
        } else {
            form
        };

        let form = if silent {
            form.text("disable_notification", "true")
        } else {
            form
        };

        let form = if let Some(id) = reply_to {
            form.text("reply_to_message_id", id.to_string())
        } else {
            form
        };

        let resp: TelegramResponse<Message> = self
            .client
            .post(self.api_url("sendDocument"))
            .multipart(form)
            .send()
            .context("Failed to send document to Telegram")?
            .json()
            .context("Failed to parse Telegram sendDocument response")?;

        if !resp.ok {
            bail!(
                "Telegram sendDocument failed: {}",
                resp.description.unwrap_or_default()
            );
        }

        resp.result.context("Telegram returned ok but no result")
    }

    /// Call Telegram getFile API to resolve a file_id to a file_path.
    pub fn get_file(&self, file_id: &str) -> Result<TelegramFile> {
        let url = format!("{}?file_id={file_id}", self.api_url("getFile"));

        let resp: TelegramResponse<TelegramFile> = self
            .client
            .get(&url)
            .send()
            .context("Failed to call Telegram getFile")?
            .json()
            .context("Failed to parse Telegram getFile response")?;

        if !resp.ok {
            bail!(
                "Telegram getFile failed: {}",
                resp.description.unwrap_or_default()
            );
        }

        resp.result.context("Telegram returned ok but no result")
    }

    /// Download a file from Telegram's file server to a local path.
    pub fn download_file(&self, file_path: &str, dest: &Path) -> Result<()> {
        let url = format!(
            "https://api.telegram.org/file/bot{}/{file_path}",
            self.token
        );

        let resp = self
            .client
            .get(&url)
            .send()
            .context("Failed to download file from Telegram")?;

        if !resp.status().is_success() {
            bail!(
                "Telegram file download failed with status: {}",
                resp.status()
            );
        }

        let bytes = resp
            .bytes()
            .context("Failed to read downloaded file bytes")?;

        let mut file = std::fs::File::create(dest)
            .with_context(|| format!("Failed to create file at {}", dest.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("Failed to write file at {}", dest.display()))?;

        Ok(())
    }

    /// Get bot info via getMe.
    pub fn get_me(&self) -> Result<User> {
        let resp: TelegramResponse<User> = self
            .client
            .get(self.api_url("getMe"))
            .send()
            .context("Failed to call Telegram getMe")?
            .json()
            .context("Failed to parse Telegram getMe response")?;

        if !resp.ok {
            bail!(
                "Telegram getMe failed: {}",
                resp.description.unwrap_or_default()
            );
        }

        resp.result.context("Telegram returned ok but no result")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // C1: send_text must serialise its body WITHOUT a `parse_mode` field, so
    // Anthropic-generated content with unbalanced `_`/`*`/backticks/`[`/`]`
    // never trips MarkdownV1 entity parsing. Compare to send_message which
    // intentionally retains MarkdownV1.
    #[test]
    fn send_text_omits_parse_mode() {
        let plain = SendMessageRequest {
            chat_id: "1",
            text: "[lotus] do_thing(*ptr) — `code`",
            parse_mode: None,
            disable_notification: None,
            reply_to_message_id: None,
        };
        let json = serde_json::to_value(&plain).unwrap();
        assert!(
            json.get("parse_mode").is_none(),
            "send_text JSON must not include parse_mode"
        );
        assert_eq!(json["text"], "[lotus] do_thing(*ptr) — `code`");
    }

    #[test]
    fn send_message_keeps_parse_mode_markdown() {
        let md = SendMessageRequest {
            chat_id: "1",
            text: "ok",
            parse_mode: Some("Markdown"),
            disable_notification: None,
            reply_to_message_id: None,
        };
        let json = serde_json::to_value(&md).unwrap();
        assert_eq!(json["parse_mode"], "Markdown");
    }

    #[test]
    fn edit_text_omits_parse_mode() {
        let plain = EditMessageTextRequest {
            chat_id: "1",
            message_id: 42,
            text: "[banshee] _underscore_at_split",
            parse_mode: None,
        };
        let json = serde_json::to_value(&plain).unwrap();
        assert!(json.get("parse_mode").is_none());
    }

    #[test]
    fn deserialize_getme_response() {
        let json = r#"{
            "ok": true,
            "result": {
                "id": 42,
                "is_bot": true,
                "first_name": "Vox Bot",
                "username": "voxbot"
            }
        }"#;
        let resp: TelegramResponse<User> = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        let user = resp.result.unwrap();
        assert_eq!(user.id, 42);
        assert!(user.is_bot);
        assert_eq!(user.username.as_deref(), Some("voxbot"));
    }

    #[test]
    fn deserialize_error_response() {
        let json = r#"{
            "ok": false,
            "description": "Unauthorized"
        }"#;
        let resp: TelegramResponse<User> = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.description.as_deref(), Some("Unauthorized"));
        assert!(resp.result.is_none());
    }

    #[test]
    fn deserialize_message_with_reply() {
        let json = r#"{
            "ok": true,
            "result": {
                "message_id": 2,
                "from": {"id": 1, "first_name": "Alice"},
                "chat": {"id": -1001, "type": "private"},
                "text": "yes",
                "reply_to_message": {
                    "message_id": 1,
                    "chat": {"id": -1001, "type": "private"},
                    "text": "deploy?"
                }
            }
        }"#;
        let resp: TelegramResponse<Message> = serde_json::from_str(json).unwrap();
        let msg = resp.result.unwrap();
        assert_eq!(msg.message_id, 2);
        assert_eq!(msg.text.as_deref(), Some("yes"));
        let parent = msg.reply_to_message.unwrap();
        assert_eq!(parent.message_id, 1);
        assert_eq!(parent.text.as_deref(), Some("deploy?"));
    }

    #[test]
    fn deserialize_update_with_callback_query() {
        let json = r#"{
            "update_id": 100,
            "callback_query": {
                "id": "cb123",
                "from": {"id": 7, "first_name": "Bob"},
                "message": {
                    "message_id": 5,
                    "chat": {"id": -1001, "type": "private"}
                },
                "data": "yes"
            }
        }"#;
        let update: Update = serde_json::from_str(json).unwrap();
        assert_eq!(update.update_id, 100);
        assert!(update.message.is_none());
        let cb = update.callback_query.unwrap();
        assert_eq!(cb.id, "cb123");
        assert_eq!(cb.data.as_deref(), Some("yes"));
        assert_eq!(cb.message.unwrap().message_id, 5);
    }

    #[test]
    fn deserialize_message_with_document() {
        let json = r#"{
            "message_id": 9,
            "chat": {"id": -1001, "type": "private"},
            "document": {
                "file_id": "abc",
                "file_name": "report.pdf",
                "mime_type": "application/pdf",
                "file_size": 1024
            }
        }"#;
        let msg: Message = serde_json::from_str(json).unwrap();
        let doc = msg.document.unwrap();
        assert_eq!(doc.file_id, "abc");
        assert_eq!(doc.file_name.as_deref(), Some("report.pdf"));
    }

    #[test]
    fn deserialize_message_with_photo_sizes() {
        let json = r#"{
            "message_id": 10,
            "chat": {"id": -1001, "type": "private"},
            "photo": [
                {"file_id": "small", "width": 90, "height": 60},
                {"file_id": "large", "width": 1280, "height": 720, "file_size": 50000}
            ]
        }"#;
        let msg: Message = serde_json::from_str(json).unwrap();
        let photos = msg.photo.unwrap();
        assert_eq!(photos.len(), 2);
        assert_eq!(photos.last().unwrap().file_id, "large");
    }
}
