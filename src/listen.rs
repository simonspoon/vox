//! Listen loop — Telegram polling + aspect-chat routing + legacy launchagent
//! coexistence (S02, S05, S06, S07).
//!
//! Routing rules (v1 vox-aspect-chat, 2026-05-09):
//!
//! - Rule 1: sender NOT in `user_id_allowlist` → `dispatch::dispatch_task`
//!   (legacy launchagent path; back-compat for agent/cron senders).
//! - Rule 2: sender allowlisted + system command → handled inline. Recognised
//!   commands: `/aspect`, `/aspect help`, `/reset`, `/new <slug>`. Synchronous,
//!   not enqueued.
//! - Rule 3: sender allowlisted + prefix grammar → aspect-chat. Primary form
//!   `<slug>: <body>` (any slug shape). Secondary form `/<slug> <body>` for
//!   no-hyphen slugs only; `_` is aliased to `-` per devil-FM4.
//! - Rule 4: sender allowlisted + no prefix → aspect-chat with sticky aspect
//!   for this chat (default `lotus`).
//!
//! BREAKING CHANGE (S07-AC3): pre-v1 agent scripts that sent task messages
//! from Simon's Telegram user_id (i.e. an *allowlisted* sender) used to hit
//! the launchagent path with no prefix. They now hit aspect-chat (rule 4)
//! instead. Migration options: prefix tasks with `lotus: <task>` (still
//! reaches lotus, just via the direct Anthropic chat path rather than
//! launchagent); or send from a non-allowlisted Telegram account so rule 1
//! still applies. Empty `user_id_allowlist` preserves pre-v1 behaviour: rule
//! 1 fires for every sender (no one is "in" an empty allowlist).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use anyhow::Result;

use crate::aspect::{self, AspectReplyResult};
use crate::config;
use crate::dispatch;
use crate::session::{ChatMessage, SessionStore};
use crate::telegram::{TelegramClient, Update};

// ── Queue state ────────────────────────────────────────────────────────────────

/// Estimated minutes per session — used for queue ETA acks.
const ESTIMATED_MINS_PER_SESSION: usize = 5;

/// Telegram message length cap — chunked replies stay ≤ this many bytes
/// (S05-AC4). Bytes ≤ chars ≤ Telegram's 4096 limit for ASCII; for Unicode
/// content this is a conservative safe upper bound.
const TELEGRAM_MSG_MAX: usize = 4096;

/// Maximum slug length accepted by `parse_prefix` (M3, code review
/// 019e0eb7-06b9). Caps the slug at 32 bytes so common prose patterns like
/// `https://...`, `note: ...`, `todo: ...`, `re: ...` don't get parsed as
/// aspect addresses just because they happen to contain a colon.
const MAX_SLUG_LEN: usize = 32;

/// System command parsed from a Telegram message.
#[derive(Debug, Clone, PartialEq)]
pub enum SystemCmd {
    Aspect,
    AspectHelp,
    Reset,
    New(String),
}

/// Routing decision for one inbound message.
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// Aspect chat with explicit slug + body. Slug may be the sticky default.
    Aspect(String, String),
    /// System command — handled synchronously, not enqueued.
    System(SystemCmd),
    /// Legacy launchagent dispatch — back-compat for non-allowlisted senders.
    Dispatch(String),
}

/// A task waiting in the dispatch queue. Carries its routing decision so that
/// queue-drain dispatch knows which path to invoke.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedTask {
    pub update_id: i64,
    pub chat_id: i64,
    pub message_id: i64,
    pub from: String,
    pub route: Route,
}

#[derive(Debug, Default)]
pub struct ListenState {
    pub task_queue: VecDeque<QueuedTask>,
    pub active_session: bool,
}

impl ListenState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process an incoming message. Returns `(ack_text, should_dispatch_now)`.
    /// Ack must be sent BEFORE any blocking dispatch (HF4).
    pub fn on_message(&mut self, task: QueuedTask) -> (String, bool) {
        if !self.active_session {
            self.active_session = true;
            ("📥 received".to_string(), true)
        } else {
            let n_ahead = 1 + self.task_queue.len();
            let m = n_ahead * ESTIMATED_MINS_PER_SESSION;
            self.task_queue.push_back(task);
            (format!("📥 queued: {} ahead, ~{} min", n_ahead, m), false)
        }
    }

    /// Called when an active session (launchagent child OR aspect API thread)
    /// completes. Pops the next task FIFO; returns `None` and marks idle when
    /// the queue is empty.
    pub fn on_session_complete(&mut self) -> Option<QueuedTask> {
        match self.task_queue.pop_front() {
            Some(next) => Some(next),
            None => {
                self.active_session = false;
                None
            }
        }
    }
}

// ── Allow-list / sender ───────────────────────────────────────────────────────

/// Numeric Telegram user_id of the sender of an update (message or callback).
pub fn sender_id(update: &Update) -> Option<i64> {
    update
        .message
        .as_ref()
        .and_then(|m| m.from.as_ref())
        .map(|u| u.id)
        .or_else(|| update.callback_query.as_ref().map(|cb| cb.from.id))
}

/// Strict membership check: returns true iff `user_id` is explicitly in
/// `allowlist`. An empty allowlist means *no one is allowlisted* (preserves
/// pre-v1 behaviour where every sender hit dispatch_task).
pub fn is_allowlisted(user_id: Option<i64>, allowlist: &[i64]) -> bool {
    match user_id {
        Some(id) => allowlist.contains(&id),
        None => false,
    }
}

/// Legacy "is this sender allowed to dispatch at all?" predicate. Empty
/// allowlist → true (all allowed). Kept for backwards compatibility with
/// existing tests; new routing uses `is_allowlisted`.
#[allow(dead_code)]
pub fn sender_allowed(update: &Update, allowlist: &[i64]) -> bool {
    if allowlist.is_empty() {
        return true;
    }
    is_allowlisted(sender_id(update), allowlist)
}

// ── Routing ───────────────────────────────────────────────────────────────────

/// Parse a system command from message text. Recognises `/aspect`,
/// `/aspect help`, `/reset`, `/new <slug>`. Anything else returns None.
pub fn parse_system_cmd(text: &str) -> Option<SystemCmd> {
    let t = text.trim();
    if t == "/aspect" {
        return Some(SystemCmd::Aspect);
    }
    if let Some(rest) = t.strip_prefix("/aspect ") {
        let rest = rest.trim();
        if rest == "help" {
            return Some(SystemCmd::AspectHelp);
        }
        // Unknown subcommand — fall through to None so prefix-routing can try.
        return None;
    }
    if t == "/reset" {
        return Some(SystemCmd::Reset);
    }
    if let Some(rest) = t.strip_prefix("/new ") {
        let slug = rest.trim();
        if !slug.is_empty() {
            return Some(SystemCmd::New(slug.to_string()));
        }
    }
    None
}

fn looks_like_slug_primary(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Parse aspect-chat prefix grammar:
///   primary:  `<slug>: <body>`         (any [a-z0-9-]+ slug, ≤32 bytes)
///   secondary: `/<word> <body>`         (alphanumeric + `_` only; FM4 alias `_`→`-`)
///
/// System command keywords (`aspect`, `reset`, `new`) are excluded from the
/// secondary form so that `/aspect <unknown>` doesn't accidentally route to
/// an aspect literally named "aspect".
///
/// M3 (code review 019e0eb7-06b9): slug is capped at `MAX_SLUG_LEN` bytes so
/// `https://foo`, `note: bought milk`, etc. don't match. Even when they pass
/// the syntactic check, callers should still validate the slug exists in
/// simaris (via `aspect_exists`) before dispatching.
pub fn parse_prefix(text: &str) -> Option<(String, String)> {
    let t = text.trim_start();
    // Primary: <slug>: <body>
    if let Some(idx) = t.find(':')
        && idx <= MAX_SLUG_LEN
        && let slug = &t[..idx]
        && looks_like_slug_primary(slug)
    {
        let rest = t[idx + 1..].trim_start();
        if !rest.is_empty() {
            return Some((slug.to_string(), rest.to_string()));
        }
    }
    // Secondary: /<word> <body>
    if let Some(rest) = t.strip_prefix('/')
        && let Some(sp) = rest.find(char::is_whitespace)
    {
        let cmd = &rest[..sp];
        let body = rest[sp + 1..].trim_start();
        if !cmd.is_empty()
            && cmd.len() <= MAX_SLUG_LEN
            && cmd.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && cmd != "aspect"
            && cmd != "reset"
            && cmd != "new"
            && !body.is_empty()
        {
            // FM4: lower-case + underscore → hyphen aliasing.
            let slug = cmd.to_lowercase().replace('_', "-");
            return Some((slug, body.to_string()));
        }
    }
    None
}

/// Compute the routing decision for a message.
///
/// `aspect_exists` is a predicate that returns true iff a simaris aspect with
/// the given slug exists. M3 (code review 019e0eb7-06b9): when the syntactic
/// prefix parse extracts a slug but no such aspect exists, the message is
/// treated as plain prose for the sticky aspect — preventing common patterns
/// like `https://foo`, `note: bought milk` from surfacing simaris errors.
pub fn route(
    text: &str,
    chat_id: i64,
    sender: Option<i64>,
    allowlist: &[i64],
    sessions: &SessionStore,
    aspect_exists: impl Fn(&str) -> bool,
) -> Route {
    if !is_allowlisted(sender, allowlist) {
        return Route::Dispatch(text.to_string());
    }
    if let Some(sys) = parse_system_cmd(text) {
        return Route::System(sys);
    }
    if let Some((slug, body)) = parse_prefix(text)
        && aspect_exists(&slug)
    {
        return Route::Aspect(slug, body);
    }
    Route::Aspect(sessions.sticky(chat_id).to_string(), text.to_string())
}

// ── Chunking (S05-AC4) ────────────────────────────────────────────────────────

/// Split `body` into chunks each prefixed with `prefix` and total length ≤ `max`.
/// Prefer split boundaries: newline > sentence (`.?!`) > whitespace > hard cut.
/// Char-boundary safe.
pub fn chunk_with_prefix(prefix: &str, body: &str, max: usize) -> Vec<String> {
    if body.is_empty() {
        return vec![prefix.to_string()];
    }
    let inner_cap = max.saturating_sub(prefix.len());
    if inner_cap == 0 {
        // Pathological: prefix already fills the budget; return prefix only.
        return vec![prefix.to_string()];
    }
    let mut out = Vec::new();
    let mut rest: &str = body;
    while !rest.is_empty() {
        if rest.len() <= inner_cap {
            out.push(format!("{prefix}{rest}"));
            break;
        }
        // Find a char-boundary candidate end ≤ inner_cap.
        let mut end = inner_cap.min(rest.len());
        while end > 0 && !rest.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            // No char boundary at or below inner_cap (single huge UTF-8 char) —
            // grow forward to next boundary to make minimum forward progress.
            let mut e = 1;
            while e < rest.len() && !rest.is_char_boundary(e) {
                e += 1;
            }
            end = e;
        }
        let candidate = &rest[..end];
        let split = find_split(candidate);
        let (head, tail) = rest.split_at(split);
        out.push(format!("{prefix}{head}"));
        rest = tail.trim_start();
    }
    out
}

fn find_split(s: &str) -> usize {
    if let Some(i) = s.rfind('\n') {
        return i + 1;
    }
    if let Some(i) = s.rfind(['.', '?', '!']) {
        return i + 1;
    }
    if let Some(i) = s.rfind(char::is_whitespace) {
        return i + 1;
    }
    s.len()
}

// ── Reply delivery (S05) ──────────────────────────────────────────────────────

fn aspect_help_text() -> String {
    // S06-AC5 + S07-AC4: documents both the prefix grammar and the agent-script
    // migration story. Sent verbatim; backticks render as inline code on
    // Telegram Markdown.
    "Address an aspect: `<slug>: msg` (any slug) or `/<slug> msg` (no-hyphen \
slugs only; use underscore alias for hyphenated slugs, e.g. \
`/vox_aspect_chat msg` → `vox-aspect-chat`).\n\n\
No prefix → routes to current sticky aspect for this chat (default `lotus`).\n\n\
Commands: `/aspect` shows current sticky · `/reset` clears history · \
`/new <slug>` switches sticky and clears prior history.\n\n\
Agent scripts: use `lotus: <task>` prefix or send from a non-allowlisted \
Telegram account to use the legacy launchagent dispatch path."
        .to_string()
}

fn handle_system_cmd(
    client: &TelegramClient,
    sessions: &mut SessionStore,
    msg_chat_id: i64,
    reply_to: i64,
    cmd: &SystemCmd,
) {
    let chat_id_str = msg_chat_id.to_string();
    let reply: String = match cmd {
        SystemCmd::Aspect => format!("Current aspect: {}", sessions.sticky(msg_chat_id)),
        SystemCmd::AspectHelp => aspect_help_text(),
        SystemCmd::Reset => {
            let slug = sessions.sticky(msg_chat_id).to_string();
            // M1: clear_history bumps the generation; an in-flight thread
            // holding the prior generation will be dropped on delivery.
            sessions.clear_history(msg_chat_id, &slug);
            if let Err(e) = sessions.save() {
                eprintln!("[system] save sessions error: {e}");
            }
            format!("[{slug}] history cleared")
        }
        SystemCmd::New(slug) => {
            if !aspect::aspect_exists(slug) {
                format!("Unknown aspect: {slug}")
            } else {
                sessions.set_sticky(msg_chat_id, slug);
                // M1: bump the new slug's generation too, so any in-flight
                // call against an older sticky-pointed `<old>` stays effective
                // for `<old>` but a concurrent in-flight call against `<slug>`
                // (e.g. user dispatched, immediately /new'd back to it) is
                // also dropped cleanly.
                sessions.clear_history(msg_chat_id, slug);
                if let Err(e) = sessions.save() {
                    eprintln!("[system] save sessions error: {e}");
                }
                format!("Switched to [{slug}]")
            }
        }
    };
    // C1: system replies contain `[<slug>]` brackets, which MarkdownV1 parses
    // as link openers and rejects when no matching `(...)` follows. Send as
    // plain text instead.
    if let Err(e) = client.send_text(&chat_id_str, &reply, false, Some(reply_to)) {
        eprintln!("[system] reply send error: {e}");
    }
}

fn deliver_aspect_reply(
    client: &TelegramClient,
    sessions: &mut SessionStore,
    r: AspectReplyResult,
) {
    let prefix = format!("[{}] ", r.slug);
    let chat_id_str = r.chat_id.to_string();
    // M1: drop the result if a /reset or /new bumped the generation while the
    // call was in flight. The user already saw their reset confirmation; the
    // late assistant turn must not silently re-create the cleared history.
    let current_gen = sessions.generation(r.chat_id, &r.slug);
    if current_gen != r.generation {
        eprintln!(
            "[aspect] dropping stale turn for {slug} (chat={chat}): \
             dispatch gen={old}, current gen={new} — reset during in-flight call",
            slug = r.slug,
            chat = r.chat_id,
            old = r.generation,
            new = current_gen,
        );
        // Best-effort: clear the `<slug> thinking…` interstitial so the user
        // doesn't stare at it. Plain text avoids MarkdownV1 hazards.
        if r.thinking_msg_id > 0 {
            let notice = format!("{prefix}reset during call — turn discarded");
            if let Err(e) = client.edit_text(&chat_id_str, r.thinking_msg_id, &notice) {
                eprintln!("[aspect] stale-turn notice edit error: {e}");
            }
        }
        return;
    }

    match r.result {
        Ok(reply) => {
            // S03-AC5: append turn + persist BEFORE we attempt Telegram sends.
            sessions.append_turn(r.chat_id, &r.slug, &r.user_text, &reply);
            if let Err(e) = sessions.save() {
                eprintln!("[aspect] save sessions error: {e}");
            }
            let chunks = chunk_with_prefix(&prefix, &reply, TELEGRAM_MSG_MAX);
            if chunks.is_empty() {
                return;
            }
            // M4: spawn the chunk-delivery loop in a background thread so the
            // 50 ms inter-chunk gaps and per-message HTTP latency don't block
            // the main `cmd_listen` poll loop. The TelegramClient is cheaply
            // Cloneable (refcounted reqwest::blocking::Client + token String).
            let client_c = client.clone();
            let prefix_c = prefix.clone();
            let slug_c = r.slug.clone();
            let chat_id_str_c = chat_id_str.clone();
            let thinking_id = r.thinking_msg_id;
            thread::spawn(move || {
                deliver_chunks_blocking(
                    &client_c,
                    &chat_id_str_c,
                    thinking_id,
                    &prefix_c,
                    &slug_c,
                    chunks,
                );
            });
        }
        Err(reason) => {
            // S03-AC4: structured error reply, no panic, no halt. C1: plain
            // text to dodge MarkdownV1 hazards in the error reason itself.
            let err_text = format!("{prefix}error: {reason}");
            if let Err(e) = client.edit_text(&chat_id_str, r.thinking_msg_id, &err_text) {
                eprintln!("[aspect] edit-error error: {e} — falling back to sendMessage");
                let _ = client.send_text(&chat_id_str, &err_text, false, None);
            }
        }
    }
}

/// Blocking chunk delivery — runs in a worker thread spawned from
/// `deliver_aspect_reply` (M4). Tracks per-chunk failures and emits a
/// `[<slug>] ⚠ partial reply (chunk K of N failed: ...)` follow-up if any
/// chunk failed (M5, code review 019e0eb7-06b9). All sends use plain text so
/// MarkdownV1 entity errors can't silently truncate the response (C1).
fn deliver_chunks_blocking(
    client: &TelegramClient,
    chat_id_str: &str,
    thinking_msg_id: i64,
    prefix: &str,
    slug: &str,
    chunks: Vec<String>,
) {
    if chunks.is_empty() {
        return;
    }
    let total = chunks.len();
    let mut failures: Vec<(usize, String)> = Vec::new();

    // S05-AC2: edit the `thinking…` message in-place with the first chunk.
    if let Err(e) = client.edit_text(chat_id_str, thinking_msg_id, &chunks[0]) {
        eprintln!("[aspect] edit error: {e} — falling back to sendMessage");
        if let Err(e2) = client.send_text(chat_id_str, &chunks[0], false, None) {
            failures.push((1, format!("edit: {e}; fallback send: {e2}")));
        }
    }
    for (idx, chunk) in chunks.iter().enumerate().skip(1) {
        // Tiny inter-chunk gap to avoid bursting Telegram's per-chat rate
        // limit on rapid sendMessage calls.
        thread::sleep(Duration::from_millis(50));
        if let Err(e) = client.send_text(chat_id_str, chunk, false, None) {
            eprintln!("[aspect] chunk {} send error: {e}", idx + 1);
            failures.push((idx + 1, e.to_string()));
        }
    }

    // M5: if any chunk failed, surface a single partial-reply notice rather
    // than leaving the user with a partial reply and no signal.
    if !failures.is_empty() {
        let (k, ref reason) = failures[0];
        let notice = format!(
            "{prefix}⚠ partial reply: chunk {k} of {total} failed: {reason}{more}",
            more = if failures.len() > 1 {
                format!(" (+{} more failures, see logs)", failures.len() - 1)
            } else {
                String::new()
            }
        );
        thread::sleep(Duration::from_millis(50));
        if let Err(e) = client.send_text(chat_id_str, &notice, false, None) {
            eprintln!("[aspect] partial-reply notice send error for {slug}: {e}");
        }
    }
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

fn dispatch_route(
    task: &QueuedTask,
    client: &TelegramClient,
    sessions: &SessionStore,
    anthropic_key: &str,
    tx: &mpsc::Sender<AspectReplyResult>,
    children: &Arc<Mutex<Vec<std::process::Child>>>,
) {
    match &task.route {
        Route::Dispatch(text) => match dispatch::dispatch_task(text, task.update_id) {
            Ok(child) => children.lock().unwrap().push(child),
            Err(e) => eprintln!("[dispatch] launchagent error: {e}"),
        },
        Route::Aspect(slug, body) => {
            // M1: capture the (chat, slug) generation BEFORE sending the
            // interstitial so a /reset during the API call can be detected on
            // delivery.
            let dispatch_gen = sessions.generation(task.chat_id, slug);
            // S05-AC1 + C1: send `<slug> thinking…` interstitial immediately so
            // the user sees a "received" signal even on long API calls. Plain
            // text — no MarkdownV1 hazards on the slug or trailing ellipsis.
            let interstitial = format!("{slug} thinking…");
            let thinking =
                match client.send_text(&task.chat_id.to_string(), &interstitial, false, None) {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("[aspect] interstitial send error: {e}");
                        let _ = tx.send(AspectReplyResult {
                            chat_id: task.chat_id,
                            thinking_msg_id: -1,
                            slug: slug.clone(),
                            user_text: body.clone(),
                            result: Err(format!("interstitial-send: {e}")),
                            generation: dispatch_gen,
                        });
                        return;
                    }
                };
            // Snapshot history (devil-A1: plain Rust value, no shell quoting).
            let mut history = sessions.history(task.chat_id, slug);
            history.push(ChatMessage {
                role: "user".into(),
                content: body.clone(),
            });
            // M4 (devil): re-load system prompt fresh from simaris on every
            // turn.
            let system_prompt = match aspect::load_system_prompt(slug) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[aspect] simaris load error for {slug}: {e}");
                    // FM3: surface as structured reply, not panic.
                    let _ = tx.send(AspectReplyResult {
                        chat_id: task.chat_id,
                        thinking_msg_id: thinking.message_id,
                        slug: slug.clone(),
                        user_text: body.clone(),
                        result: Err(format!("simaris show: {e}")),
                        generation: dispatch_gen,
                    });
                    return;
                }
            };
            // FM5: spawn API call in a background thread; mpsc sends result
            // back to the main thread. Telegram polling continues unblocked.
            let key = anthropic_key.to_string();
            let slug_c = slug.clone();
            let body_c = body.clone();
            let chat_id = task.chat_id;
            let thinking_id = thinking.message_id;
            let tx_c = tx.clone();
            thread::spawn(move || {
                let result = aspect::call_anthropic(&key, &system_prompt, &history)
                    .map_err(|e| e.to_string());
                let _ = tx_c.send(AspectReplyResult {
                    chat_id,
                    thinking_msg_id: thinking_id,
                    slug: slug_c,
                    user_text: body_c,
                    result,
                    generation: dispatch_gen,
                });
            });
        }
        Route::System(_) => {
            // System commands are handled inline at receive-time — never queued.
            eprintln!("[dispatch] BUG: system command reached queue dispatch");
        }
    }
}

// ── Main loop ─────────────────────────────────────────────────────────────────

/// Long-poll loop: polls getUpdates forever, routing per the rules at top of file.
///
/// `--foreground` is accepted for launchd compatibility but is a no-op in v1
/// (we always run in the foreground; daemonisation is out of scope).
pub fn cmd_listen(_foreground: bool) -> Result<()> {
    let cfg = config::load_config()?;
    let token = config::load_token()?;
    // S01-AC4: fail fast with a clear error if the Anthropic key isn't set.
    let anthropic_key = config::load_anthropic_key()?;
    let client = TelegramClient::new(token);
    let mut sessions = SessionStore::load()?;

    eprintln!("[vox listen] polling chat_id={} …", cfg.chat_id);

    let mut offset: Option<i64> = None;
    let mut state = ListenState::new();

    let children: Arc<Mutex<Vec<std::process::Child>>> = Arc::new(Mutex::new(Vec::new()));
    let children_sig = Arc::clone(&children);

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_sig = Arc::clone(&shutdown);

    // mpsc channel for aspect API thread completions.
    let (tx, rx) = mpsc::channel::<AspectReplyResult>();

    ctrlc::set_handler(move || {
        eprintln!("[vox listen] signal received — shutting down");
        shutdown_sig.store(true, Ordering::Relaxed);
        if let Ok(mut ch) = children_sig.lock() {
            for child in ch.iter_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
            ch.clear();
        }
        std::process::exit(0);
    })?;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // ── Reap completed launchagent children (legacy path) ──────────────
        let mut next_tasks: Vec<QueuedTask> = Vec::new();
        {
            let mut ch = children.lock().unwrap();
            let mut i = 0;
            while i < ch.len() {
                match ch[i].try_wait() {
                    Ok(Some(_status)) => {
                        // try_wait returning Some has already reaped the child;
                        // explicit wait() afterwards is a no-op but appeases
                        // clippy::zombie_processes.
                        let mut child = ch.remove(i);
                        let _ = child.wait();
                        if let Some(next) = state.on_session_complete() {
                            next_tasks.push(next);
                        }
                    }
                    Ok(None) => i += 1,
                    Err(e) => {
                        eprintln!("[dispatch] try_wait error: {e}");
                        let mut child = ch.remove(i);
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                }
            }
        }

        // ── Reap completed aspect API threads (drain channel) ──────────────
        while let Ok(reply) = rx.try_recv() {
            deliver_aspect_reply(&client, &mut sessions, reply);
            if let Some(next) = state.on_session_complete() {
                next_tasks.push(next);
            }
        }

        // ── Dispatch popped-off-queue tasks (outside the children mutex) ───
        for task in next_tasks {
            dispatch_route(&task, &client, &sessions, &anthropic_key, &tx, &children);
        }

        // ── Poll Telegram (no lock held) ───────────────────────────────────
        let updates = client.get_updates(offset, 30)?;

        // ── Process updates ────────────────────────────────────────────────
        let mut to_dispatch: Vec<QueuedTask> = Vec::new();
        for update in &updates {
            offset = Some(update.update_id + 1);

            if let Some(ref msg) = update.message {
                let from = msg
                    .from
                    .as_ref()
                    .map(|u| u.first_name.as_str())
                    .unwrap_or("unknown");
                let text = msg.text.as_deref().unwrap_or("<non-text>");
                eprintln!(
                    "[update {}] message from={} chat={} text={:?}",
                    update.update_id, from, msg.chat.id, text
                );

                let s_id = sender_id(update);
                let route_decision = route(
                    text,
                    msg.chat.id,
                    s_id,
                    &cfg.user_id_allowlist,
                    &sessions,
                    aspect::aspect_exists,
                );

                // Rule 2: system commands are synchronous and bypass the queue.
                if let Route::System(ref cmd) = route_decision {
                    handle_system_cmd(&client, &mut sessions, msg.chat.id, msg.message_id, cmd);
                    continue;
                }

                let task = QueuedTask {
                    update_id: update.update_id,
                    chat_id: msg.chat.id,
                    message_id: msg.message_id,
                    from: from.to_string(),
                    route: route_decision,
                };

                let (ack, dispatch_now) = state.on_message(task.clone());
                eprintln!("[update {}] ack: {}", update.update_id, ack);
                // Fire-and-forget ack — sent before any blocking dispatch.
                let _ = client.send_message(
                    &msg.chat.id.to_string(),
                    &ack,
                    false,
                    Some(msg.message_id),
                );

                if dispatch_now {
                    to_dispatch.push(task);
                }
            } else if let Some(ref cb) = update.callback_query {
                eprintln!(
                    "[update {}] callback_query id={} data={:?}",
                    update.update_id,
                    cb.id,
                    cb.data.as_deref().unwrap_or("")
                );
            } else {
                eprintln!("[update {}] unknown update type", update.update_id);
            }
        }

        for task in to_dispatch {
            dispatch_route(&task, &client, &sessions, &anthropic_key, &tx, &children);
        }
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::{Chat, Message, Update, User};

    fn make_update(user_id: i64) -> Update {
        Update {
            update_id: 1,
            message: Some(Message {
                message_id: 1,
                from: Some(User {
                    id: user_id,
                    is_bot: false,
                    first_name: "Test".to_string(),
                    last_name: None,
                    username: Some("testuser".to_string()),
                }),
                chat: Chat {
                    id: 999,
                    chat_type: "private".to_string(),
                    first_name: None,
                    last_name: None,
                    username: None,
                },
                text: Some("hello".to_string()),
                reply_to_message: None,
                document: None,
                photo: None,
            }),
            callback_query: None,
        }
    }

    fn task(id: i64, body: &str) -> QueuedTask {
        QueuedTask {
            update_id: id,
            chat_id: 999,
            message_id: id,
            from: "Test".to_string(),
            route: Route::Aspect("lotus".to_string(), body.to_string()),
        }
    }

    // ── Existing tests preserved ─────────────────────────────────────────

    // S01-U01: unknown sender → not allowed (legacy non-empty allowlist).
    #[test]
    fn allowlist_unknown_sender_blocked() {
        let allowlist = vec![111111i64, 222222i64];
        let update = make_update(999999);
        assert!(!sender_allowed(&update, &allowlist));
    }

    // S01-U02: known sender → allowed.
    #[test]
    fn allowlist_known_sender_allowed() {
        let allowlist = vec![111111i64, 222222i64];
        let update = make_update(111111);
        assert!(sender_allowed(&update, &allowlist));
    }

    // S01-U03: HF2 — user_id (numeric) checked, not username.
    #[test]
    fn allowlist_checks_numeric_id_not_username() {
        let allowlist = vec![111111i64];
        let update = make_update(999999);
        assert!(!sender_allowed(&update, &allowlist));
        let update_known = make_update(111111);
        assert!(sender_allowed(&update_known, &allowlist));
    }

    // Empty allowlist + sender_allowed = true (legacy compat).
    #[test]
    fn empty_allowlist_legacy_allows_all() {
        let update = make_update(999999);
        assert!(sender_allowed(&update, &[]));
    }

    // Strict is_allowlisted: empty allowlist = nobody is allowlisted.
    #[test]
    fn empty_allowlist_strict_excludes_all() {
        assert!(!is_allowlisted(Some(999999), &[]));
    }

    // S01-U04..U06: queue ack semantics unchanged.
    #[test]
    fn queue_position() {
        let mut state = ListenState::new();

        let (ack1, dispatch1) = state.on_message(task(1, "first"));
        assert_eq!(ack1, "📥 received", "S01-U06: received ack");
        assert!(dispatch1, "first message should dispatch immediately");
        assert!(state.active_session);
        assert!(state.task_queue.is_empty());

        let (ack2, dispatch2) = state.on_message(task(2, "second"));
        assert_eq!(ack2, "📥 queued: 1 ahead, ~5 min", "S01-U05: queue ack");
        assert!(!dispatch2);
        assert_eq!(state.task_queue.len(), 1);

        let (ack3, dispatch3) = state.on_message(task(3, "third"));
        assert_eq!(
            ack3, "📥 queued: 2 ahead, ~10 min",
            "S01-U05: queue ack pos 2"
        );
        assert!(!dispatch3);
        assert_eq!(state.task_queue.len(), 2);

        assert_eq!(state.task_queue.front().map(|t| t.update_id), Some(2));
        assert_eq!(state.task_queue.back().map(|t| t.update_id), Some(3));
    }

    // S01-U10: queue drain — pop-and-dispatch on session complete, FIFO order.
    #[test]
    fn queue_dispatches_on_complete() {
        let mut state = ListenState::new();

        state.active_session = true;
        state.task_queue.push_back(task(2, "second"));
        state.task_queue.push_back(task(3, "third"));

        let next = state.on_session_complete();
        assert!(next.is_some());
        assert_eq!(next.unwrap().update_id, 2);
        assert!(state.active_session);
        assert_eq!(state.task_queue.len(), 1);

        let next = state.on_session_complete();
        assert!(next.is_some());
        assert_eq!(next.unwrap().update_id, 3);
        assert!(state.active_session);
        assert!(state.task_queue.is_empty());

        let next = state.on_session_complete();
        assert!(next.is_none());
        assert!(!state.active_session);
    }

    #[test]
    fn shutdown_flag_works() {
        let flag = Arc::new(AtomicBool::new(false));
        assert!(!flag.load(Ordering::Relaxed));
        flag.store(true, Ordering::Relaxed);
        assert!(flag.load(Ordering::Relaxed));
    }

    #[test]
    fn child_set_arc_mutex_accessible() {
        let children: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
        let children_clone = Arc::clone(&children);
        {
            let mut ch = children_clone.lock().unwrap();
            ch.push(42);
        }
        let ch = children.lock().unwrap();
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0], 42);
    }

    // ── S02 prefix grammar ───────────────────────────────────────────────

    // S02-AC1: `banshee: <text>` → Aspect("banshee", "<text>").
    #[test]
    fn parse_prefix_primary_form() {
        let p = parse_prefix("banshee: ping").unwrap();
        assert_eq!(p.0, "banshee");
        assert_eq!(p.1, "ping");
    }

    // S02-AC1: hyphenated slug works in primary form.
    #[test]
    fn parse_prefix_primary_with_hyphen() {
        let p = parse_prefix("vox-aspect-chat: hello there").unwrap();
        assert_eq!(p.0, "vox-aspect-chat");
        assert_eq!(p.1, "hello there");
    }

    // S02-AC2: `/banshee <text>` → Aspect("banshee", "<text>").
    #[test]
    fn parse_prefix_secondary_form() {
        let p = parse_prefix("/banshee ping").unwrap();
        assert_eq!(p.0, "banshee");
        assert_eq!(p.1, "ping");
    }

    // S02-AC3 / FM4: `/vox_aspect_chat <text>` → ("vox-aspect-chat", "<text>").
    #[test]
    fn parse_prefix_secondary_underscore_alias() {
        let p = parse_prefix("/vox_aspect_chat hello").unwrap();
        assert_eq!(p.0, "vox-aspect-chat");
        assert_eq!(p.1, "hello");
    }

    #[test]
    fn parse_prefix_no_match_returns_none() {
        assert!(parse_prefix("plain message").is_none());
        assert!(
            parse_prefix("Foo: not a slug").is_none(),
            "uppercase rejected"
        );
        assert!(parse_prefix("banshee:").is_none(), "empty body rejected");
        assert!(parse_prefix("/banshee").is_none(), "no body rejected");
    }

    // System command keywords are excluded from prefix grammar.
    #[test]
    fn parse_prefix_excludes_system_keywords() {
        assert!(parse_prefix("/aspect foo").is_none());
        assert!(parse_prefix("/reset foo").is_none());
        assert!(parse_prefix("/new foo").is_none());
    }

    // M3: slugs longer than MAX_SLUG_LEN are rejected at the parse layer even
    // if otherwise valid characters. URLs like `https://...` parse
    // syntactically (slug = "https") but are caught at the `route()` layer by
    // the `aspect_exists` predicate — see `route_url_falls_through_to_sticky`.
    #[test]
    fn parse_prefix_rejects_long_slug() {
        let long = "a".repeat(MAX_SLUG_LEN + 1);
        let text = format!("{long}: body");
        assert!(
            parse_prefix(&text).is_none(),
            "{}-char slug must reject",
            long.len()
        );
    }

    // M3 + boundary: exactly MAX_SLUG_LEN chars is still accepted.
    #[test]
    fn parse_prefix_accepts_max_len_slug() {
        let max = "a".repeat(MAX_SLUG_LEN);
        let text = format!("{max}: body");
        let p = parse_prefix(&text).expect("max-len slug must parse");
        assert_eq!(p.0.len(), MAX_SLUG_LEN);
        assert_eq!(p.1, "body");
    }

    // ── S06 system commands ──────────────────────────────────────────────

    #[test]
    fn parse_system_aspect() {
        assert_eq!(parse_system_cmd("/aspect"), Some(SystemCmd::Aspect));
        assert_eq!(parse_system_cmd("  /aspect  "), Some(SystemCmd::Aspect));
    }

    #[test]
    fn parse_system_aspect_help() {
        assert_eq!(
            parse_system_cmd("/aspect help"),
            Some(SystemCmd::AspectHelp)
        );
    }

    #[test]
    fn parse_system_aspect_unknown_subcmd_returns_none() {
        // Falls back so prefix-routing or sticky can take over if appropriate.
        assert_eq!(parse_system_cmd("/aspect foo"), None);
    }

    #[test]
    fn parse_system_reset() {
        assert_eq!(parse_system_cmd("/reset"), Some(SystemCmd::Reset));
    }

    #[test]
    fn parse_system_new() {
        assert_eq!(
            parse_system_cmd("/new banshee"),
            Some(SystemCmd::New("banshee".to_string()))
        );
        assert_eq!(
            parse_system_cmd("/new vox-aspect-chat"),
            Some(SystemCmd::New("vox-aspect-chat".to_string()))
        );
    }

    #[test]
    fn parse_system_new_empty_slug_returns_none() {
        assert_eq!(parse_system_cmd("/new "), None);
    }

    // ── route() rules ────────────────────────────────────────────────────

    /// Stub aspect_exists used by routing tests: pretends `lotus`, `banshee`,
    /// and `vox-aspect-chat` exist; everything else does not.
    fn known_aspect(slug: &str) -> bool {
        matches!(slug, "lotus" | "banshee" | "vox-aspect-chat")
    }

    #[test]
    fn route_non_allowlisted_sender_dispatches_legacy() {
        let allowlist = vec![1i64];
        let sessions = SessionStore::default();
        let r = route(
            "banshee: hi",
            99,
            Some(2),
            &allowlist,
            &sessions,
            known_aspect,
        );
        assert_eq!(r, Route::Dispatch("banshee: hi".to_string()));
    }

    #[test]
    fn route_empty_allowlist_dispatches_legacy() {
        // Devil-S07 / breaking-change doc: empty allowlist preserves legacy.
        let sessions = SessionStore::default();
        let r = route("banshee: hi", 99, Some(2), &[], &sessions, known_aspect);
        assert_eq!(r, Route::Dispatch("banshee: hi".to_string()));
    }

    #[test]
    fn route_allowlisted_with_prefix_aspect_chat() {
        let allowlist = vec![1i64];
        let sessions = SessionStore::default();
        let r = route(
            "banshee: ping",
            99,
            Some(1),
            &allowlist,
            &sessions,
            known_aspect,
        );
        assert_eq!(r, Route::Aspect("banshee".to_string(), "ping".to_string()));
    }

    #[test]
    fn route_allowlisted_no_prefix_uses_sticky_default_lotus() {
        let allowlist = vec![1i64];
        let sessions = SessionStore::default();
        let r = route(
            "just a chat",
            99,
            Some(1),
            &allowlist,
            &sessions,
            known_aspect,
        );
        assert_eq!(
            r,
            Route::Aspect("lotus".to_string(), "just a chat".to_string())
        );
    }

    #[test]
    fn route_allowlisted_no_prefix_uses_sticky_when_set() {
        let allowlist = vec![1i64];
        let mut sessions = SessionStore::default();
        sessions.set_sticky(99, "banshee");
        let r = route(
            "just a chat",
            99,
            Some(1),
            &allowlist,
            &sessions,
            known_aspect,
        );
        assert_eq!(
            r,
            Route::Aspect("banshee".to_string(), "just a chat".to_string())
        );
    }

    #[test]
    fn route_allowlisted_system_command() {
        let allowlist = vec![1i64];
        let sessions = SessionStore::default();
        let r = route("/aspect", 99, Some(1), &allowlist, &sessions, known_aspect);
        assert_eq!(r, Route::System(SystemCmd::Aspect));
    }

    // M3: prefix that parses syntactically but doesn't match any known aspect
    // falls through to sticky with the original text intact.
    #[test]
    fn route_unknown_slug_falls_through_to_sticky() {
        let allowlist = vec![1i64];
        let sessions = SessionStore::default();
        // `note:` parses as a slug syntactically but no `note` aspect exists.
        let r = route(
            "note: bought milk",
            99,
            Some(1),
            &allowlist,
            &sessions,
            known_aspect,
        );
        assert_eq!(
            r,
            Route::Aspect("lotus".to_string(), "note: bought milk".to_string())
        );
    }

    // M3: a URL pasted by mistake never routes to a non-existent aspect.
    #[test]
    fn route_url_falls_through_to_sticky() {
        let allowlist = vec![1i64];
        let sessions = SessionStore::default();
        let r = route(
            "https://example.com/foo",
            99,
            Some(1),
            &allowlist,
            &sessions,
            known_aspect,
        );
        assert_eq!(
            r,
            Route::Aspect("lotus".to_string(), "https://example.com/foo".to_string())
        );
    }

    // ── S05 chunking ─────────────────────────────────────────────────────

    #[test]
    fn chunk_short_body_one_chunk() {
        let chunks = chunk_with_prefix("[lotus] ", "hello", 4096);
        assert_eq!(chunks, vec!["[lotus] hello".to_string()]);
    }

    #[test]
    fn chunk_long_body_multiple_chunks_each_under_max() {
        let body: String = "a".repeat(10_000);
        let chunks = chunk_with_prefix("[x] ", &body, 4096);
        assert!(chunks.len() >= 3);
        for c in &chunks {
            assert!(c.len() <= 4096, "chunk len {} exceeds 4096", c.len());
            assert!(c.starts_with("[x] "), "chunk missing prefix");
        }
        // Concatenated bodies (minus prefix) reconstruct the original.
        let recovered: String = chunks.iter().map(|c| &c[4..]).collect();
        assert_eq!(recovered, body);
    }

    #[test]
    fn chunk_prefers_newline_split() {
        let body = format!("{}\n{}", "x".repeat(100), "y".repeat(50));
        let chunks = chunk_with_prefix("[p] ", &body, 110); // tiny max to force split
        // First chunk must end at the newline (i.e. only x's, no y's).
        assert!(chunks[0].contains('x'));
        assert!(!chunks[0].contains('y'));
    }

    #[test]
    fn chunk_empty_body_emits_prefix_only() {
        let chunks = chunk_with_prefix("[lotus] ", "", 4096);
        assert_eq!(chunks, vec!["[lotus] ".to_string()]);
    }
}
