use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub chat_id: String,
}

/// Returns the config file path: ~/.config/vox/config.toml
pub fn config_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir()
        .context("Could not determine config directory")?
        .join("vox");
    Ok(config_dir.join("config.toml"))
}

/// Load config from ~/.config/vox/config.toml
pub fn load_config() -> Result<Config> {
    let path = config_path()?;
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("Could not read config at {path:?}. Run `vox init` first."))?;
    let config: Config = toml::from_str(&contents).context("Invalid config file format")?;
    Ok(config)
}

/// Save config to ~/.config/vox/config.toml
pub fn save_config(config: &Config) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Could not create config directory {parent:?}"))?;
    }
    let contents = toml::to_string_pretty(config).context("Could not serialize config")?;
    fs::write(&path, contents).with_context(|| format!("Could not write config to {path:?}"))?;
    Ok(())
}

/// Store the bot token in macOS Keychain via the security CLI.
pub fn store_token(token: &str) -> Result<()> {
    let output = Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-a",
            "vox",
            "-s",
            "vox-telegram-token",
            "-w",
            token,
        ])
        .output()
        .context("Failed to run `security` command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to store token in Keychain: {stderr}");
    }
    Ok(())
}

/// Load the bot token from macOS Keychain via the security CLI.
pub fn load_token() -> Result<String> {
    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-a",
            "vox",
            "-s",
            "vox-telegram-token",
            "-w",
        ])
        .output()
        .context("Failed to run `security` command")?;

    if !output.status.success() {
        bail!("No bot token found in Keychain. Run `vox init` first.");
    }

    let token = String::from_utf8(output.stdout)
        .context("Token from Keychain is not valid UTF-8")?
        .trim()
        .to_string();

    if token.is_empty() {
        bail!("Bot token in Keychain is empty. Run `vox init` to reconfigure.");
    }

    Ok(token)
}
