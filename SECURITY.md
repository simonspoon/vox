# Security Policy

## Reporting a vulnerability

Please report security issues privately via [GitHub's private vulnerability reporting](https://github.com/simonspoon/vox/security/advisories/new) rather than opening a public issue.

Include:

- A description of the issue and its impact.
- Steps to reproduce, or a proof of concept.
- Any suggested mitigation, if you have one.

## Scope

The primary sensitive asset handled by vox is your **Telegram bot token**. Vox stores it in the macOS Keychain via the `security` CLI — it is never written to a config file or logged.

- Treat the token like a password. Anyone with it can impersonate your bot and send/receive messages on its behalf.
- If you believe your token has been exposed, revoke it immediately via [@BotFather](https://t.me/BotFather) (`/revoke`) and re-run `vox init`.
- Vox reads replies from the configured `chat_id` only. If you point the bot at a shared group, every member of that group can answer its prompts.

## Supported versions

This project is pre-1.0. Only the latest release receives security fixes.
