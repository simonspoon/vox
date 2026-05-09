use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub user_id_allowlist: Vec<i64>,
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
///
/// M2 (code review 019e0eb7-06b9): the secret is fed to `security` via stdin
/// rather than as a `-w <secret>` argv element, so it's not visible to other
/// processes via `ps -axww`. `security add-generic-password -w` (without a
/// value) prompts for the password twice on stdin (typing + confirmation), so
/// we feed the secret + newline twice.
pub fn store_token(token: &str) -> Result<()> {
    pipe_security_password("vox-telegram-token", token).context("store bot token in Keychain")
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

// ── Anthropic API key (S01) ───────────────────────────────────────────────────

/// Store the Anthropic API key in macOS Keychain via the security CLI.
/// Mirrors `store_token`; uses service `vox-anthropic-key`, account `vox`.
/// `-U` upserts so re-running `vox init` overwrites.
///
/// M2 (code review 019e0eb7-06b9): same stdin-piped pattern as `store_token`
/// so the API key never appears on argv.
pub fn store_anthropic_key(key: &str) -> Result<()> {
    pipe_security_password("vox-anthropic-key", key).context("store Anthropic key in Keychain")
}

/// Internal helper: spawn `security add-generic-password -U -a vox -s <service>
/// -w` (no argv password) and write `<secret>\n<secret>\n` to its stdin.
/// `security` prompts for the password twice (entry + confirmation) on stdin
/// when `-w` has no value; feeding it twice satisfies both prompts. M2.
fn pipe_security_password(service: &str, secret: &str) -> Result<()> {
    use std::process::Stdio;
    let mut child = Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-a",
            "vox",
            "-s",
            service,
            "-w",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn `security` subprocess")?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .context("security stdin not captured")?;
        // Two prompts: password, then retype. Both newline-terminated.
        write!(stdin, "{secret}\n{secret}\n").context("write secret to security stdin")?;
    }
    let output = child.wait_with_output().context("wait for security")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("`security add-generic-password` failed: {}", stderr.trim());
    }
    Ok(())
}

/// Load the Anthropic API key from macOS Keychain via the security CLI.
/// Surfaces a clear error pointing back at `vox init` when absent (S01-AC4).
pub fn load_anthropic_key() -> Result<String> {
    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-a",
            "vox",
            "-s",
            "vox-anthropic-key",
            "-w",
        ])
        .output()
        .context("Failed to run `security` command")?;

    if !output.status.success() {
        bail!(
            "No Anthropic API key found in Keychain (service `vox-anthropic-key`). \
             Run `vox init` and provide an Anthropic key when prompted."
        );
    }

    let key = String::from_utf8(output.stdout)
        .context("Anthropic key from Keychain is not valid UTF-8")?
        .trim()
        .to_string();

    if key.is_empty() {
        bail!("Anthropic key in Keychain is empty. Run `vox init` to reconfigure.");
    }

    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    // S01-U07: allowlist parses correctly with values
    #[test]
    fn user_id_allowlist_parses_with_values() {
        let toml = r#"
chat_id = "test_chat"
user_id_allowlist = [123456789, 987654321]
"#;
        let config: Config = toml::from_str(toml).expect("parse failed");
        assert_eq!(config.user_id_allowlist, vec![123456789i64, 987654321i64]);
    }

    // S01-U08: allowlist defaults to empty vec when absent
    #[test]
    fn user_id_allowlist_defaults_to_empty() {
        let toml = r#"
chat_id = "test_chat"
"#;
        let config: Config = toml::from_str(toml).expect("parse failed");
        assert!(config.user_id_allowlist.is_empty());
    }

    // S01-U09: allowlist with single entry parses correctly
    #[test]
    fn user_id_allowlist_single_entry() {
        let toml = r#"
chat_id = "test_chat"
user_id_allowlist = [123456789]
"#;
        let config: Config = toml::from_str(toml).expect("parse failed");
        assert_eq!(config.user_id_allowlist, vec![123456789i64]);
        assert_eq!(config.user_id_allowlist.len(), 1);
    }
}
