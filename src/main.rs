mod aspect;
mod config;
mod dispatch;
mod display;
mod listen;
mod session;
mod telegram;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::io::{self, BufRead};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "vox", about = "Telegram messaging CLI for Claude Code agents")]
struct Cli {
    /// Output results as JSON
    #[arg(long, global = true)]
    json: bool,

    /// Send messages without notification sound
    #[arg(long, global = true)]
    silent: bool,

    /// Render agent identity in the message title slot
    #[arg(long = "as", global = true)]
    as_name: Option<String>,

    /// Render a distinct title above the message body
    #[arg(long, global = true)]
    title: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Interactive setup -- configure bot token and chat ID
    Init,

    /// Send a message and wait for a reply
    Ask {
        /// The question to send
        message: String,

        /// Timeout duration (e.g., "30m", "1h", "2h")
        #[arg(long, default_value = "1h")]
        timeout: String,

        /// Default value to return if timeout expires
        #[arg(long)]
        on_timeout: Option<String>,

        /// Reply to a specific message ID for threading
        #[arg(long)]
        reply_to: Option<i64>,

        /// Add a URL button as "Label=URL" (repeatable, one button per row)
        #[arg(long = "button", value_parser = telegram::parse_button)]
        buttons: Vec<telegram::InlineKeyboardButton>,
    },

    /// Send a fire-and-forget notification
    Notify {
        /// The message to send
        message: String,

        /// Reply to a specific message ID for threading
        #[arg(long)]
        reply_to: Option<i64>,

        /// Add a URL button as "Label=URL" (repeatable, one button per row)
        #[arg(long = "button", value_parser = telegram::parse_button)]
        buttons: Vec<telegram::InlineKeyboardButton>,
    },

    /// Send a yes/no question with inline keyboard buttons and wait for a response
    Confirm {
        /// The yes/no question to send
        message: String,

        /// Timeout duration (e.g., "30m", "1h")
        #[arg(long, default_value = "1h")]
        timeout: String,

        /// Default value to return if timeout expires (must be "yes" or "no")
        #[arg(long)]
        on_timeout: Option<String>,

        /// Reply to a specific message ID for threading
        #[arg(long)]
        reply_to: Option<i64>,

        /// Add a URL button as "Label=URL" (repeatable, one button per row)
        #[arg(long = "button", value_parser = telegram::parse_button)]
        buttons: Vec<telegram::InlineKeyboardButton>,
    },

    /// Send a multiple-choice question and wait for selection
    Pick {
        /// The question to send
        message: String,

        /// Comma-separated options
        #[arg(short, long)]
        options: String,

        /// Timeout duration (e.g., "30m", "1h", "2h")
        #[arg(long, default_value = "1h")]
        timeout: String,

        /// Default value to return if timeout expires (must match one of the options)
        #[arg(long)]
        on_timeout: Option<String>,

        /// Reply to a specific message ID for threading
        #[arg(long)]
        reply_to: Option<i64>,
    },

    /// Send a file to the chat
    Send {
        /// Path to the file to send
        path: PathBuf,

        /// Optional caption for the file
        #[arg(long)]
        caption: Option<String>,

        /// Reply to a specific message ID for threading
        #[arg(long)]
        reply_to: Option<i64>,
    },

    /// Edit an existing message in-place
    Progress {
        /// Message ID to edit
        message_id: i64,
        /// New message text
        text: String,
    },

    /// Wait for any message from the user
    Wait {
        /// Timeout duration (e.g., "30m", "1h", "2h")
        #[arg(long, default_value = "1h")]
        timeout: String,

        /// Default value to return if timeout expires
        #[arg(long)]
        on_timeout: Option<String>,
    },

    /// Poll for incoming Telegram updates and log them to stderr
    Listen {
        /// Run in the foreground (no daemonization; for launchd compatibility)
        #[arg(long)]
        foreground: bool,
    },

    /// Check bot connectivity and config
    Status,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let as_name = cli.as_name.as_deref();
    let title = cli.title.as_deref();

    match cli.command {
        Commands::Init => cmd_init()?,
        Commands::Ask {
            message,
            timeout,
            on_timeout,
            reply_to,
            buttons,
        } => cmd_ask(
            &message,
            &timeout,
            on_timeout.as_deref(),
            cli.json,
            cli.silent,
            as_name,
            title,
            reply_to,
            buttons,
        )?,
        Commands::Confirm {
            message,
            timeout,
            on_timeout,
            reply_to,
            buttons,
        } => cmd_confirm(
            &message,
            &timeout,
            on_timeout.as_deref(),
            cli.json,
            cli.silent,
            as_name,
            title,
            reply_to,
            buttons,
        )?,
        Commands::Pick {
            message,
            options,
            timeout,
            on_timeout,
            reply_to,
        } => cmd_pick(
            &message,
            &options,
            &timeout,
            on_timeout.as_deref(),
            cli.json,
            cli.silent,
            as_name,
            title,
            reply_to,
        )?,
        Commands::Progress { message_id, text } => {
            cmd_progress(message_id, &text, cli.json, as_name)?
        }
        Commands::Notify {
            message,
            reply_to,
            buttons,
        } => cmd_notify(
            &message, cli.json, cli.silent, as_name, title, reply_to, buttons,
        )?,
        Commands::Send {
            path,
            caption,
            reply_to,
        } => cmd_send(
            &path,
            caption.as_deref(),
            cli.json,
            cli.silent,
            as_name,
            title,
            reply_to,
        )?,
        Commands::Wait {
            timeout,
            on_timeout,
        } => cmd_wait(&timeout, on_timeout.as_deref(), cli.json)?,
        Commands::Listen { foreground } => listen::cmd_listen(foreground)?,
        Commands::Status => cmd_status(cli.json)?,
    }

    Ok(())
}

fn cmd_init() -> Result<()> {
    eprintln!("Enter your Telegram bot token:");

    let stdin = io::stdin();
    let token = stdin
        .lock()
        .lines()
        .next()
        .context("No input received")?
        .context("Failed to read from stdin")?
        .trim()
        .to_string();

    if token.is_empty() {
        bail!("Bot token cannot be empty.");
    }

    // Verify token by calling getMe
    let client = telegram::TelegramClient::new(token.clone());
    let user = client
        .get_me()
        .context("Token verification failed. Is the token correct?")?;

    // Store token in Keychain
    config::store_token(&token)?;

    // Prompt for chat ID
    eprint!("Chat ID: ");
    let mut chat_id = String::new();
    std::io::stdin().read_line(&mut chat_id)?;
    let chat_id = chat_id.trim().to_string();
    anyhow::ensure!(!chat_id.is_empty(), "Chat ID cannot be empty");

    // S01: Anthropic API key (Keychain only — never written to config.toml)
    eprint!("Anthropic API key: ");
    let mut anth_key = String::new();
    std::io::stdin().read_line(&mut anth_key)?;
    let anth_key = anth_key.trim().to_string();
    anyhow::ensure!(!anth_key.is_empty(), "Anthropic API key cannot be empty");
    config::store_anthropic_key(&anth_key)?;

    let cfg = config::Config {
        chat_id,
        user_id_allowlist: Vec::new(),
    };
    config::save_config(&cfg)?;

    display::print_init_success(&user);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_ask(
    message: &str,
    timeout_str: &str,
    on_timeout: Option<&str>,
    json_output: bool,
    silent: bool,
    as_name: Option<&str>,
    title: Option<&str>,
    reply_to: Option<i64>,
    buttons: Vec<telegram::InlineKeyboardButton>,
) -> Result<()> {
    let cfg = config::load_config()?;
    let token = config::load_token()?;
    let client = telegram::TelegramClient::new(token);

    let timeout = parse_duration(timeout_str)?;

    // Send the question
    let display_msg = apply_identity(message, as_name, title);
    let sent = send_with_buttons(
        &client,
        &cfg.chat_id,
        &display_msg,
        silent,
        reply_to,
        buttons,
    )?;
    let sent_id = sent.message_id;

    if !json_output {
        eprintln!("Waiting for reply (timeout: {timeout_str})...");
    }

    // Poll for reply
    let start = Instant::now();
    let mut offset: Option<i64> = None;

    loop {
        if start.elapsed() >= timeout {
            if let Some(default) = on_timeout {
                eprintln!("Timed out, using default: {default}");
                display::print_reply_default(default, json_output);
                return Ok(());
            }
            bail!("Timed out waiting for reply after {timeout_str}");
        }

        let updates = client.get_updates(offset, 30)?;

        for update in &updates {
            // Advance offset past this update
            offset = Some(update.update_id + 1);

            if let Some(ref msg) = update.message
                && msg.chat.id.to_string() == cfg.chat_id
                && let Some(ref reply_to) = msg.reply_to_message
                && reply_to.message_id == sent_id
            {
                let file = download_message_file(&client, msg)?;
                if let Some(off) = offset {
                    client.commit_offset(off);
                }
                display::print_reply(msg, file.as_deref(), json_output, false);
                return Ok(());
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_confirm(
    message: &str,
    timeout_str: &str,
    on_timeout: Option<&str>,
    json_output: bool,
    silent: bool,
    as_name: Option<&str>,
    title: Option<&str>,
    reply_to: Option<i64>,
    buttons: Vec<telegram::InlineKeyboardButton>,
) -> Result<()> {
    // Validate on_timeout value before sending the message
    if let Some(val) = on_timeout
        && val != "yes"
        && val != "no"
    {
        bail!("--on-timeout for confirm must be \"yes\" or \"no\", got: {val:?}");
    }

    let cfg = config::load_config()?;
    let token = config::load_token()?;
    let client = telegram::TelegramClient::new(token);

    let timeout = parse_duration(timeout_str)?;

    // Build inline keyboard with Yes/No buttons
    let mut keyboard = telegram::InlineKeyboardMarkup {
        inline_keyboard: vec![vec![
            telegram::InlineKeyboardButton {
                text: "Yes".to_string(),
                callback_data: Some("yes".to_string()),
                url: None,
            },
            telegram::InlineKeyboardButton {
                text: "No".to_string(),
                callback_data: Some("no".to_string()),
                url: None,
            },
        ]],
    };
    // URL buttons go in extra rows below Yes/No
    keyboard
        .inline_keyboard
        .extend(buttons.into_iter().map(|b| vec![b]));

    // Send the question with keyboard
    let display_msg = apply_identity(message, as_name, title);
    let sent = client.send_message_with_keyboard(
        &cfg.chat_id,
        &display_msg,
        keyboard,
        silent,
        reply_to,
    )?;
    let sent_id = sent.message_id;

    if !json_output {
        eprintln!("Waiting for confirmation (timeout: {timeout_str})...");
    }

    // Poll for callback query response
    let start = Instant::now();
    let mut offset: Option<i64> = None;

    loop {
        if start.elapsed() >= timeout {
            if let Some(default) = on_timeout {
                eprintln!("Timed out, using default: {default}");
                display::print_confirm(default, sent_id, json_output, true);
                return Ok(());
            }
            bail!("Timed out waiting for confirmation after {timeout_str}");
        }

        let updates = client.get_updates(offset, 30)?;

        for update in &updates {
            offset = Some(update.update_id + 1);

            if let Some(ref cb) = update.callback_query
                && let Some(ref cb_msg) = cb.message
                && cb_msg.message_id == sent_id
            {
                // Acknowledge the callback to dismiss loading state
                let _ = client.answer_callback_query(&cb.id);

                if let Some(off) = offset {
                    client.commit_offset(off);
                }

                let answer = cb.data.as_deref().unwrap_or("unknown");
                display::print_confirm(answer, sent_id, json_output, false);
                return Ok(());
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_pick(
    message: &str,
    options_str: &str,
    timeout_str: &str,
    on_timeout: Option<&str>,
    json_output: bool,
    silent: bool,
    as_name: Option<&str>,
    title: Option<&str>,
    reply_to: Option<i64>,
) -> Result<()> {
    let options: Vec<String> = options_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if options.len() < 2 {
        bail!("At least 2 options are required");
    }

    // Check for duplicates
    let mut seen = std::collections::HashSet::new();
    for opt in &options {
        if !seen.insert(opt.as_str()) {
            bail!("Duplicate option: {opt}");
        }
    }

    // Validate on_timeout value matches one of the options
    if let Some(val) = on_timeout
        && !options.iter().any(|opt| opt == val)
    {
        bail!(
            "--on-timeout value {val:?} must match one of the options: {}",
            options.join(", ")
        );
    }

    let cfg = config::load_config()?;
    let token = config::load_token()?;
    let client = telegram::TelegramClient::new(token);

    let timeout = parse_duration(timeout_str)?;

    // Build keyboard: one button per row for readability
    let keyboard = telegram::InlineKeyboardMarkup {
        inline_keyboard: options
            .iter()
            .map(|opt| {
                vec![telegram::InlineKeyboardButton {
                    text: opt.clone(),
                    callback_data: Some(opt.clone()),
                    url: None,
                }]
            })
            .collect(),
    };

    let display_msg = apply_identity(message, as_name, title);
    let sent = client.send_message_with_keyboard(
        &cfg.chat_id,
        &display_msg,
        keyboard,
        silent,
        reply_to,
    )?;
    let sent_id = sent.message_id;

    if !json_output {
        eprintln!("Waiting for selection (timeout: {timeout_str})...");
    }

    let start = Instant::now();
    let mut offset: Option<i64> = None;

    loop {
        if start.elapsed() >= timeout {
            if let Some(default) = on_timeout {
                eprintln!("Timed out, using default: {default}");
                display::print_pick(default, sent_id, json_output, true);
                return Ok(());
            }
            bail!("Timed out waiting for selection after {timeout_str}");
        }

        let updates = client.get_updates(offset, 30)?;

        for update in &updates {
            offset = Some(update.update_id + 1);

            if let Some(ref cb) = update.callback_query
                && let Some(ref cb_msg) = cb.message
                && cb_msg.message_id == sent_id
            {
                let _ = client.answer_callback_query(&cb.id);

                if let Some(off) = offset {
                    client.commit_offset(off);
                }

                let choice = cb.data.as_deref().unwrap_or("unknown");
                display::print_pick(choice, sent_id, json_output, false);
                return Ok(());
            }
        }
    }
}

/// Send a plain message, or one with URL buttons (one per row) when given.
fn send_with_buttons(
    client: &telegram::TelegramClient,
    chat_id: &str,
    text: &str,
    silent: bool,
    reply_to: Option<i64>,
    buttons: Vec<telegram::InlineKeyboardButton>,
) -> Result<telegram::Message> {
    if buttons.is_empty() {
        return client.send_message(chat_id, text, silent, reply_to);
    }
    let keyboard = telegram::InlineKeyboardMarkup {
        inline_keyboard: buttons.into_iter().map(|b| vec![b]).collect(),
    };
    client.send_message_with_keyboard(chat_id, text, keyboard, silent, reply_to)
}

fn cmd_notify(
    message: &str,
    json_output: bool,
    silent: bool,
    as_name: Option<&str>,
    title: Option<&str>,
    reply_to: Option<i64>,
    buttons: Vec<telegram::InlineKeyboardButton>,
) -> Result<()> {
    let cfg = config::load_config()?;
    let token = config::load_token()?;
    let client = telegram::TelegramClient::new(token);

    let display_msg = apply_identity(message, as_name, title);
    let sent = send_with_buttons(
        &client,
        &cfg.chat_id,
        &display_msg,
        silent,
        reply_to,
        buttons,
    )?;
    display::print_sent(&sent, json_output);
    Ok(())
}

fn cmd_progress(
    message_id: i64,
    text: &str,
    json_output: bool,
    as_name: Option<&str>,
) -> Result<()> {
    let cfg = config::load_config()?;
    let token = config::load_token()?;
    let client = telegram::TelegramClient::new(token);

    let display_msg = apply_identity(text, as_name, None);
    let edited = client.edit_message_text(&cfg.chat_id, message_id, &display_msg)?;
    display::print_progress(&edited, json_output);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_send(
    path: &std::path::Path,
    caption: Option<&str>,
    json_output: bool,
    silent: bool,
    as_name: Option<&str>,
    title: Option<&str>,
    reply_to: Option<i64>,
) -> Result<()> {
    anyhow::ensure!(path.exists(), "File not found: {}", path.display());

    let cfg = config::load_config()?;
    let token = config::load_token()?;
    let client = telegram::TelegramClient::new(token);

    let display_caption = caption.map(|c| apply_identity(c, as_name, title));
    let sent = client.send_document(
        &cfg.chat_id,
        path,
        display_caption.as_deref(),
        silent,
        reply_to,
    )?;
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    display::print_file_sent(&sent, &filename, json_output);
    Ok(())
}

fn cmd_wait(timeout_str: &str, on_timeout: Option<&str>, json_output: bool) -> Result<()> {
    let cfg = config::load_config()?;
    let token = config::load_token()?;
    let client = telegram::TelegramClient::new(token);

    let timeout = parse_duration(timeout_str)?;

    // Get bot's own user ID so we can filter out our own messages
    let bot_user = client.get_me()?;
    let bot_id = bot_user.id;

    if !json_output {
        eprintln!("Waiting for message (timeout: {timeout_str})...");
    }

    let start = Instant::now();
    let mut offset: Option<i64> = None;

    loop {
        if start.elapsed() >= timeout {
            if let Some(default) = on_timeout {
                eprintln!("Timed out, using default: {default}");
                display::print_wait_default(default, json_output);
                return Ok(());
            }
            bail!("Timed out waiting for message after {timeout_str}");
        }

        let updates = client.get_updates(offset, 30)?;

        for update in &updates {
            offset = Some(update.update_id + 1);

            if let Some(ref msg) = update.message
                && msg.chat.id.to_string() == cfg.chat_id
                && msg.from.as_ref().is_some_and(|u| u.id != bot_id)
            {
                let text = msg.text.as_deref().unwrap_or("");
                let from_name = msg
                    .from
                    .as_ref()
                    .map(|u| u.first_name.as_str())
                    .unwrap_or("unknown");
                let file = download_message_file(&client, msg)?;
                if let Some(off) = offset {
                    client.commit_offset(off);
                }
                display::print_wait_result(
                    text,
                    file.as_deref(),
                    msg.message_id,
                    from_name,
                    json_output,
                );
                return Ok(());
            }
        }
    }
}

fn cmd_status(json_output: bool) -> Result<()> {
    let _cfg = config::load_config()?;
    let token = config::load_token()?;
    let client = telegram::TelegramClient::new(token);

    let user = client.get_me()?;
    display::print_status(&user, json_output);
    Ok(())
}

/// If the given message has a document or photo, download it to the vox temp
/// directory and return the local path. Otherwise returns Ok(None).
fn download_message_file(
    client: &telegram::TelegramClient,
    msg: &telegram::Message,
) -> Result<Option<PathBuf>> {
    // Get file_id from document or largest photo
    let file_id = if let Some(ref doc) = msg.document {
        Some((doc.file_id.clone(), doc.file_name.clone()))
    } else if let Some(ref photos) = msg.photo {
        // Pick largest photo (last in array -- Telegram sorts ascending)
        photos.last().map(|p| (p.file_id.clone(), None))
    } else {
        None
    };

    let Some((file_id, file_name)) = file_id else {
        return Ok(None);
    };

    let file_info = client.get_file(&file_id)?;
    let file_path = file_info
        .file_path
        .context("Telegram returned no file_path")?;

    // Create <tempdir>/vox/
    let tmp_dir = std::env::temp_dir().join("vox");
    std::fs::create_dir_all(&tmp_dir)
        .with_context(|| format!("Failed to create temp dir at {}", tmp_dir.display()))?;

    // Determine filename: use document's filename, or extract from file_path
    let dest_name = file_name.unwrap_or_else(|| {
        std::path::Path::new(&file_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("{file_id}.bin"))
    });
    let dest = tmp_dir.join(&dest_name);

    client.download_file(&file_path, &dest)?;
    Ok(Some(dest))
}

/// Build a display message by rendering the optional title slot above the body.
///
/// The title slot content is derived from (as_name, title):
/// - (None, None) -> body only, unchanged
/// - (Some(name), None) -> title is `name`
/// - (None, Some(t)) -> title is `t`
/// - (Some(name), Some(t)) -> title is `[name] t`
///
/// When present the title renders as a bold heading on its own line above the body,
/// using Telegram legacy Markdown (`*bold*`).
fn apply_identity(message: &str, as_name: Option<&str>, title: Option<&str>) -> String {
    let heading = match (as_name, title) {
        (None, None) => return message.to_string(),
        (Some(name), None) => name.to_string(),
        (None, Some(t)) => t.to_string(),
        (Some(name), Some(t)) => format!("[{name}] {t}"),
    };
    format!("*{heading}*\n{message}")
}

/// Parse a human-readable duration string like "30m", "1h", "2h".
fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        bail!("Duration cannot be empty");
    }

    let (num_str, unit) = s.split_at(s.len() - 1);
    let num: u64 = num_str
        .parse()
        .with_context(|| format!("Invalid duration number: {num_str:?}"))?;

    let secs = match unit {
        "s" => num,
        "m" => num * 60,
        "h" => num * 3600,
        _ => bail!("Unknown duration unit: {unit:?}. Use 's', 'm', or 'h'."),
    };

    Ok(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(parse_duration("15m").unwrap(), Duration::from_secs(15 * 60));
    }

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(2 * 3600));
    }

    #[test]
    fn parse_duration_trims_whitespace() {
        assert_eq!(parse_duration("  1h  ").unwrap(), Duration::from_secs(3600));
    }

    #[test]
    fn parse_duration_rejects_empty() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("   ").is_err());
    }

    #[test]
    fn parse_duration_rejects_unknown_unit() {
        assert!(parse_duration("5d").is_err());
        assert!(parse_duration("5x").is_err());
    }

    #[test]
    fn parse_duration_rejects_bad_number() {
        assert!(parse_duration("abcm").is_err());
    }

    #[test]
    fn apply_identity_neither_is_passthrough() {
        assert_eq!(apply_identity("hello", None, None), "hello");
    }

    #[test]
    fn apply_identity_as_alone_renders_name_as_title() {
        // --as alone now surfaces in the title slot. No body-prefix.
        assert_eq!(
            apply_identity("hello", Some("deploy-bot"), None),
            "*deploy-bot*\nhello"
        );
    }

    #[test]
    fn apply_identity_title_alone_renders_title() {
        assert_eq!(
            apply_identity("hello", None, Some("Deployed")),
            "*Deployed*\nhello"
        );
    }

    #[test]
    fn apply_identity_as_and_title_combine_with_bracket() {
        assert_eq!(
            apply_identity("hello", Some("deploy-bot"), Some("Deployed")),
            "*[deploy-bot] Deployed*\nhello"
        );
    }
}
