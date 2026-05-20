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
# Anthropic API key: <paste>
# Initialized vox with bot @yourbot (Your Bot)
```

The Anthropic API key is only used by `vox listen` aspect-chat (see below); the
other commands work without it, but `init` currently asks for all three values
up front.

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
| `listen` | Long-running daemon: route inbound messages to legacy task dispatch and/or aspect-chat | logs to stderr |
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
| `--as <name>` | Render the agent identity as a bold heading above the message body |
| `--title <text>` | Render a distinct title above the message body |

`--as` and `--title` both drive the same heading slot. If both are provided, the heading becomes `[name] title`. The heading renders as Telegram legacy Markdown (`*bold*`) on its own line above the body. Applies to `ask`, `confirm`, `pick`, `notify`, and `send` (as caption). `progress` respects `--as` but ignores `--title`.

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

## Listen mode (aspect chat + task dispatch)

`vox listen` is a long-running daemon that polls Telegram and routes each inbound message in one of two ways:

- **Legacy task dispatch.** Messages from a sender *not* in `user_id_allowlist` are handed to the existing launchagent task pipeline (back-compat for agent/cron senders). Empty allowlist preserves pre-v0.3 behaviour — every sender hits this path.
- **Aspect chat.** Messages from an allowlisted sender are forwarded to a per-(chat, aspect) conversation with the Anthropic API. The system prompt is reloaded fresh from `simaris show <slug> --content` on each turn, so aspect edits propagate immediately. Reply chunks are sent back as plain text prefixed with `[<slug>]`.

```sh
vox listen --foreground   # for launchd / systemd; otherwise daemonizes
```

### Addressing an aspect

| Form | Meaning |
|---|---|
| `<slug>: <body>` | Talk to `<slug>` (any slug shape, including hyphens) |
| `/<slug> <body>` | Talk to `<slug>` (no-hyphen slugs only; `_` is aliased to `-`, e.g. `/vox_aspect_chat` → `vox-aspect-chat`) |
| `<body>` (no prefix) | Routes to the *sticky* aspect for this chat (default `lotus`) |

The slug must exist in `simaris` — unknown slugs fall through to the sticky aspect with the text left intact, so prose like `note:` or `https://...` is not misparsed. Slug length is capped at 32 bytes.

### Aspect commands

| Command | Effect |
|---|---|
| `/aspect` | Show the current sticky aspect for this chat |
| `/aspect help` | Print the addressing grammar and migration notes |
| `/reset` | Clear conversation history with the current sticky aspect |
| `/new <slug>` | Switch sticky aspect and clear prior history |

History persists across restarts in `~/.config/vox/sessions.json`; entries idle more than 30 minutes are pruned.

### Allowlist

`user_id_allowlist` in `~/.config/vox/config.toml` is a list of numeric Telegram user IDs allowed to use aspect chat. Edit the file by hand:

```toml
chat_id = "123456789"
user_id_allowlist = [123456789]
```

**Breaking change for human-operated agent scripts:** before v0.3, an agent script sending a task message from an *allowlisted* account went to the launchagent path. It now goes to aspect-chat (sticky `lotus`) instead. To keep the legacy behaviour either prefix tasks with `lotus: <task>` (still reaches lotus, but via the direct Anthropic chat path) or send from a non-allowlisted account.

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
| Chat ID | `~/.config/vox/config.toml` (`chat_id`) |
| Aspect-chat allowlist | `~/.config/vox/config.toml` (`user_id_allowlist`) |
| Bot token | macOS Keychain, account `vox`, service `vox-telegram-token` |
| Anthropic API key | macOS Keychain, account `vox`, service `vox-anthropic-key` |
| Aspect-chat history | `~/.config/vox/sessions.json` (flock + atomic write) |
| Downloaded files | `$TMPDIR/vox/` |

Re-run `vox init` to re-prompt for the chat ID, bot token, and Anthropic key. Secrets are fed to `security` via stdin, never via `-w <secret>` on argv.

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
