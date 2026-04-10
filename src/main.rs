mod config;
mod display;
mod telegram;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::io::{self, BufRead};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "vox", about = "Telegram messaging CLI for Claude Code agents")]
struct Cli {
    /// Output results as JSON
    #[arg(long, global = true)]
    json: bool,

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
    },

    /// Send a fire-and-forget notification
    Notify {
        /// The message to send
        message: String,
    },

    /// Check bot connectivity and config
    Status,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init => cmd_init()?,
        Commands::Ask { message, timeout } => cmd_ask(&message, &timeout, cli.json)?,
        Commands::Notify { message } => cmd_notify(&message, cli.json)?,
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

    // Save config with hardcoded chat_id
    let cfg = config::Config {
        chat_id: "8620850843".to_string(),
    };
    config::save_config(&cfg)?;

    display::print_init_success(&user);
    Ok(())
}

fn cmd_ask(message: &str, timeout_str: &str, json_output: bool) -> Result<()> {
    let cfg = config::load_config()?;
    let token = config::load_token()?;
    let client = telegram::TelegramClient::new(token);

    let timeout = parse_duration(timeout_str)?;

    // Send the question
    let sent = client.send_message(&cfg.chat_id, message)?;
    let sent_id = sent.message_id;

    if !json_output {
        eprintln!("Waiting for reply (timeout: {timeout_str})...");
    }

    // Poll for reply
    let start = Instant::now();
    let mut offset: Option<i64> = None;

    loop {
        if start.elapsed() >= timeout {
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
                display::print_reply(msg, json_output);
                return Ok(());
            }
        }
    }
}

fn cmd_notify(message: &str, json_output: bool) -> Result<()> {
    let cfg = config::load_config()?;
    let token = config::load_token()?;
    let client = telegram::TelegramClient::new(token);

    let sent = client.send_message(&cfg.chat_id, message)?;
    display::print_sent(&sent, json_output);
    Ok(())
}

fn cmd_status(json_output: bool) -> Result<()> {
    let _cfg = config::load_config()?;
    let token = config::load_token()?;
    let client = telegram::TelegramClient::new(token);

    let user = client.get_me()?;
    display::print_status(&user, json_output);
    Ok(())
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
