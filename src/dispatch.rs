use std::process::{Child, Command};

use anyhow::{Context, Result};

/// Spawn `launchagent <task>` as a background subprocess.
/// Returns the `Child` handle; caller must poll via `try_wait` (non-blocking).
///
/// `reply_to_msg_id` is reserved for future sid-file integration.
/// TODO(ihmv): forward --sid-file <reply_to_msg_id> once sid-file polling lands.
pub fn dispatch_task(task: &str, _reply_to_msg_id: i64) -> Result<Child> {
    Command::new("launchagent")
        .arg(task)
        .spawn()
        .context("failed to spawn launchagent — ensure it is in PATH")
}
