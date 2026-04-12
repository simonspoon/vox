# vox

Async Telegram bridge for background [Claude Code](https://claude.com/claude-code) agents.

Lets a non-interactive agent send a question to your Telegram, block until you reply, and read your answer back from stdout. Useful when a long-running agent needs a human decision without watching a terminal.

```
agent  ──vox ask──►  Telegram  ──user reply──►  stdout  ──►  agent resumes
```

## Status

Early, works-for-me. macOS only — the bot token is stored in the macOS Keychain via the `security` CLI (`src/config.rs`). Linux/Windows support would require a different token store (e.g. the `keyring` crate); PRs welcome.

## Install

```sh
cargo install --git https://github.com/simonspoon/vox
```

Or from source:

```sh
git clone https://github.com/simonspoon/vox
cd vox
cargo install --path .
```

## Setup

1. Create a bot with [@BotFather](https://t.me/BotFather) and copy the token.
2. Start a chat with your new bot (send it any message) so it can message you.
3. Get your chat ID. Easiest route: message [@getmyid_bot](https://t.me/getmyid_bot), or open `https://api.telegram.org/bot<TOKEN>/getUpdates` in a browser after sending a message.
4. Run `vox init` and paste both values when prompted.

```sh
vox init
# Enter your Telegram bot token: <paste>
# Chat ID: <paste>
# Initialized vox with bot @yourbot (Your Bot)
```

Verify:

```sh
vox status
```

## Commands

| Command | Purpose | Stdout on success |
|---|---|---|
| `init` | Interactive setup | human-readable |
| `ask <msg>` | Send a question, block for a reply | reply text (or file path) |
| `confirm <msg>` | Yes/No inline buttons, block for tap | `yes` or `no` |
| `pick <msg> -o a,b,c` | Multiple-choice buttons, block for tap | chosen option |
| `notify <msg>` | Fire-and-forget message | sent message id |
| `send <path>` | Upload a file | sent message id |
| `progress <id> <text>` | Edit an existing message in place | edited message id |
| `wait` | Block for the next user message | message text (or file path) |
| `status` | Check bot connectivity | bot info |

All reply-returning commands write the raw answer to stdout with no trailing newline, so they compose with shell substitution:

```sh
answer=$(vox ask "Should I deploy to prod?")
```

### Global flags

| Flag | Effect |
|---|---|
| `--json` | Emit JSON instead of plain text |
| `--silent` | Send without notification sound |
| `--as <name>` | Prefix the message with `**[name]**` so the user knows which agent is asking |

### Blocking commands — timeouts

`ask`, `confirm`, `pick`, and `wait` accept:

- `--timeout <dur>` — duration like `30s`, `15m`, `2h`. Default `1h`.
- `--on-timeout <value>` — default value to print on timeout instead of failing. For `confirm` it must be `yes` or `no`; for `pick` it must match one of the options.

Without `--on-timeout`, a timeout is a non-zero exit.

### Threading

`ask`, `confirm`, `pick`, `notify`, and `send` accept `--reply-to <message-id>` to thread replies under an earlier message, so multi-step flows stay visually grouped in Telegram.

## Examples

Ask a free-form question with a one-hour timeout, defaulting to "skip":

```sh
plan=$(vox ask "What should I do about the failing test?" \
  --timeout 1h --on-timeout "skip" --as "tech-lead")
```

Yes/No gate before a destructive action:

```sh
if [ "$(vox confirm 'Delete the stale branch?' --timeout 10m --on-timeout no)" = "yes" ]; then
  git branch -D stale-branch
fi
```

Multiple-choice routing:

```sh
route=$(vox pick "Which reviewer?" -o "alice,bob,skip" --timeout 30m --on-timeout skip)
```

Live progress updates on a single message:

```sh
id=$(vox notify "Build starting..." --json | jq -r .message_id)
vox progress "$id" "Build: compiling"
vox progress "$id" "Build: testing"
vox progress "$id" "Build: done"
```

Send a report file:

```sh
vox send ./report.pdf --caption "Nightly run complete"
```

Wait for the next thing the user says (no prompt sent):

```sh
next=$(vox wait --timeout 30m)
```

### File replies

If the user's reply to `ask` (or the next message picked up by `wait`) is a photo or document, vox downloads it to `$TMPDIR/vox/<filename>` and prints the local path to stdout instead of text. Callers can branch on whether the output is a path that exists.

## Claude Code integration

Drop a shell wrapper into an agent's toolkit so it can pause for human input:

```sh
# .claude/agents/deploy.sh
set -e

build
if [ "$(vox confirm 'Build passed. Ship to prod?' --as deploy-bot --on-timeout no)" = "yes" ]; then
  deploy
  vox notify "Deploy complete" --as deploy-bot
else
  vox notify "Deploy aborted by user" --as deploy-bot
fi
```

Because `vox ask`/`confirm`/`pick`/`wait` block the process, a background agent spawned by Claude Code will sit quietly until you respond — no polling logic in the agent itself.

## Configuration & storage

| What | Where |
|---|---|
| Chat ID | `~/.config/vox/config.toml` |
| Bot token | macOS Keychain, account `vox`, service `vox-telegram-token` |
| Downloaded files | `$TMPDIR/vox/` |

Re-run `vox init` to change either value.

## Building

```sh
cargo build --release
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test
```

Requires Rust 2024 edition (stable). CI runs the same checks on `ubuntu-latest` and `macos-latest`.

## Security notes

- The bot token grants full control of your bot — treat it like a password. It is stored via `security add-generic-password`, not in the config file.
- vox only reads messages from the configured `chat_id`. If you point the bot at a shared group, every member of that group can answer its prompts.
- Messages are sent over HTTPS to `api.telegram.org`.

## License

MIT — see [LICENSE](LICENSE).
