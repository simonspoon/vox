use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Telegram Bot API client.
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
pub struct Message {
    pub message_id: i64,
    pub from: Option<User>,
    pub chat: Chat,
    pub text: Option<String>,
    pub reply_to_message: Option<Box<Message>>,
}

#[derive(Debug, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
}

#[derive(Debug, Serialize)]
struct SendMessageRequest<'a> {
    chat_id: &'a str,
    text: &'a str,
    parse_mode: &'a str,
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

    /// Send a message to a chat. Returns the sent Message.
    pub fn send_message(&self, chat_id: &str, text: &str) -> Result<Message> {
        let body = SendMessageRequest {
            chat_id,
            text,
            parse_mode: "Markdown",
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
        params.push("allowed_updates=[\"message\"]".to_string());
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
