//! Session persistence (S04).
//!
//! Stores per-(chat_id, aspect_slug) message history plus per-chat sticky aspect
//! at `~/.config/vox/sessions.json`. Concurrent-write safety via advisory `flock`
//! on `~/.config/vox/sessions.lock` and atomic temp-file rename on save.
//!
//! Devil-A3: dev-daemon overlap can not corrupt the file — exclusive flock + rename
//! is atomic on the same filesystem; readers see either the old or new file in full.
//!
//! Stale entries (`last_active` older than `TTL_SECS`) are pruned on load.

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 30 minutes — entries older than this are pruned on load.
pub const TTL_SECS: u64 = 1800;

/// Soft history cap per (chat_id, slug): keep the most recent N messages.
/// 80 messages = 40 user/assistant pairs. M6 (code review 019e0eb7-06b9):
/// prevents unbounded growth that would eventually overflow the model's
/// context window with a `prompt is too long` error.
pub const HISTORY_MAX_MESSAGES: usize = 80;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String, // "user" | "assistant"
    pub content: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AspectSession {
    #[serde(default)]
    pub history: Vec<ChatMessage>,
    #[serde(default)]
    pub last_active: u64,
    /// M1 (code review 019e0eb7-06b9): bumped on every history-clearing event
    /// (`/reset`, `/new <slug>`). The listen loop captures this value at
    /// dispatch time and a stale (mismatched) result is dropped before any
    /// `append_turn` so a `/reset` issued during an in-flight API call is
    /// never silently undone.
    #[serde(default)]
    pub generation: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ChatState {
    #[serde(default)]
    pub sticky_aspect: Option<String>,
    #[serde(default)]
    pub aspects: HashMap<String, AspectSession>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SessionStore {
    #[serde(default)]
    pub chats: HashMap<i64, ChatState>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn config_dir() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("could not determine config directory")?
        .join("vox"))
}

pub fn sessions_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("sessions.json"))
}

pub fn lock_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("sessions.lock"))
}

fn ensure_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create dir {:?}", parent))?;
    }
    Ok(())
}

fn open_lock(lock: &Path) -> Result<File> {
    ensure_dir(lock)?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock)
        .with_context(|| format!("open lock file {:?}", lock))
}

fn prune(store: &mut SessionStore) {
    let now = now_secs();
    for chat in store.chats.values_mut() {
        chat.aspects
            .retain(|_, sess| now.saturating_sub(sess.last_active) <= TTL_SECS);
    }
}

impl SessionStore {
    /// Load from default `sessions.json`. Missing/empty/malformed → default.
    pub fn load() -> Result<SessionStore> {
        let data = sessions_path()?;
        let lock = lock_path()?;
        Self::load_from(&data, &lock)
    }

    /// Save to default `sessions.json` atomically, exclusive-locked.
    pub fn save(&self) -> Result<()> {
        let data = sessions_path()?;
        let lock = lock_path()?;
        self.save_to(&data, &lock)
    }

    /// Test-friendly variant: load from explicit data + lock paths.
    pub fn load_from(data: &Path, lock: &Path) -> Result<SessionStore> {
        let lock_file = open_lock(lock)?;
        FileExt::lock_shared(&lock_file).context("flock shared")?;
        let mut store: SessionStore = match fs::read_to_string(data) {
            Ok(ref s) if s.trim().is_empty() => SessionStore::default(),
            Ok(ref s) => serde_json::from_str(s).unwrap_or_default(),
            Err(_) => SessionStore::default(),
        };
        let _ = FileExt::unlock(&lock_file);
        prune(&mut store);
        Ok(store)
    }

    /// Test-friendly variant: save to explicit data + lock paths.
    pub fn save_to(&self, data: &Path, lock: &Path) -> Result<()> {
        ensure_dir(data)?;
        let lock_file = open_lock(lock)?;
        FileExt::lock_exclusive(&lock_file).context("flock exclusive")?;

        let tmp = data.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(self).context("serialize sessions")?;
        {
            let mut f = File::create(&tmp).with_context(|| format!("create {:?}", tmp))?;
            f.write_all(&json)
                .with_context(|| format!("write {:?}", tmp))?;
            f.sync_all().ok();
        }
        fs::rename(&tmp, data).with_context(|| format!("rename {:?} -> {:?}", tmp, data))?;

        let _ = FileExt::unlock(&lock_file);
        Ok(())
    }

    /// Sticky aspect for a chat, defaulting to `"lotus"` per S04-AC2 / direction.
    pub fn sticky(&self, chat_id: i64) -> &str {
        self.chats
            .get(&chat_id)
            .and_then(|c| c.sticky_aspect.as_deref())
            .unwrap_or("lotus")
    }

    pub fn set_sticky(&mut self, chat_id: i64, slug: &str) {
        let c = self.chats.entry(chat_id).or_default();
        c.sticky_aspect = Some(slug.to_string());
    }

    /// Snapshot of history for `(chat_id, slug)` — empty vec if absent.
    pub fn history(&self, chat_id: i64, slug: &str) -> Vec<ChatMessage> {
        self.chats
            .get(&chat_id)
            .and_then(|c| c.aspects.get(slug))
            .map(|s| s.history.clone())
            .unwrap_or_default()
    }

    /// Current generation for `(chat_id, slug)` — 0 if no entry exists.
    /// Captured at dispatch time; checked at delivery time to drop turns whose
    /// session was reset mid-call (M1).
    pub fn generation(&self, chat_id: i64, slug: &str) -> u64 {
        self.chats
            .get(&chat_id)
            .and_then(|c| c.aspects.get(slug))
            .map(|s| s.generation)
            .unwrap_or(0)
    }

    /// Clear history AND bump the generation counter (M1). The bump must
    /// outlive the deletion so an in-flight thread that holds the prior
    /// generation can detect the reset on delivery.
    pub fn clear_history(&mut self, chat_id: i64, slug: &str) {
        let c = self.chats.entry(chat_id).or_default();
        let prior_gen = c.aspects.get(slug).map(|s| s.generation).unwrap_or(0);
        c.aspects.remove(slug);
        // Re-insert an empty session carrying gen+1 so the next dispatch sees
        // the new generation immediately. (Without this, `generation()` would
        // return 0 until the next `append_turn`, masking concurrent resets.)
        let s = c.aspects.entry(slug.to_string()).or_default();
        s.generation = prior_gen.saturating_add(1);
        s.last_active = now_secs();
    }

    /// Append a completed user/assistant turn pair and stamp `last_active = now`.
    /// Trims to the most-recent `HISTORY_MAX_MESSAGES` (M6).
    pub fn append_turn(&mut self, chat_id: i64, slug: &str, user: &str, assistant: &str) {
        let c = self.chats.entry(chat_id).or_default();
        let s = c.aspects.entry(slug.to_string()).or_default();
        s.history.push(ChatMessage {
            role: "user".into(),
            content: user.into(),
        });
        s.history.push(ChatMessage {
            role: "assistant".into(),
            content: assistant.into(),
        });
        // M6: keep only the most recent HISTORY_MAX_MESSAGES messages, dropping
        // the oldest user/assistant pair(s) when over the cap. Anthropic
        // requires the first message to be `user`, so we drain pairs (drain
        // 2 at a time) to preserve role alternation invariant.
        while s.history.len() > HISTORY_MAX_MESSAGES {
            s.history.drain(0..2);
        }
        s.last_active = now_secs();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_paths(tag: &str) -> (PathBuf, PathBuf) {
        static N: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let n = N.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("vox-test-{tag}-{pid}-{nanos}-{n}"));
        fs::create_dir_all(&dir).unwrap();
        (dir.join("sessions.json"), dir.join("sessions.lock"))
    }

    // S04-AC1/AC2: sticky default lotus, schema serialises round-trip.
    #[test]
    fn sticky_defaults_to_lotus() {
        let store = SessionStore::default();
        assert_eq!(store.sticky(42), "lotus");
    }

    #[test]
    fn set_sticky_persists_in_memory() {
        let mut store = SessionStore::default();
        store.set_sticky(42, "banshee");
        assert_eq!(store.sticky(42), "banshee");
    }

    #[test]
    fn append_turn_stores_pair() {
        let mut store = SessionStore::default();
        store.append_turn(7, "lotus", "hi", "hello");
        let h = store.history(7, "lotus");
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].role, "user");
        assert_eq!(h[0].content, "hi");
        assert_eq!(h[1].role, "assistant");
        assert_eq!(h[1].content, "hello");
    }

    #[test]
    fn clear_history_drops_aspect() {
        let mut store = SessionStore::default();
        store.append_turn(7, "lotus", "hi", "hello");
        assert_eq!(store.history(7, "lotus").len(), 2);
        store.clear_history(7, "lotus");
        assert!(store.history(7, "lotus").is_empty());
    }

    // M1: clear_history must bump the generation so an in-flight thread can
    // detect that its captured generation is stale and drop the result.
    #[test]
    fn clear_history_bumps_generation() {
        let mut store = SessionStore::default();
        store.append_turn(7, "lotus", "hi", "hello");
        let g0 = store.generation(7, "lotus");
        store.clear_history(7, "lotus");
        let g1 = store.generation(7, "lotus");
        assert!(g1 > g0, "generation must strictly increase on reset");
    }

    #[test]
    fn generation_zero_for_unknown_chat() {
        let store = SessionStore::default();
        assert_eq!(store.generation(123, "lotus"), 0);
    }

    // M6: history is trimmed to the most-recent HISTORY_MAX_MESSAGES; oldest
    // user/assistant pairs are dropped first; role alternation preserved.
    #[test]
    fn append_turn_trims_to_history_cap() {
        let mut store = SessionStore::default();
        let cap_pairs = HISTORY_MAX_MESSAGES / 2;
        // Push (cap_pairs + 5) pairs.
        for i in 0..(cap_pairs + 5) {
            store.append_turn(1, "lotus", &format!("u{i}"), &format!("a{i}"));
        }
        let h = store.history(1, "lotus");
        assert_eq!(
            h.len(),
            HISTORY_MAX_MESSAGES,
            "history trimmed to cap exactly"
        );
        // Oldest surviving turn must be the 6th (index 5) user message.
        assert_eq!(h.first().unwrap().role, "user", "must start on user role");
        assert_eq!(h.first().unwrap().content, "u5", "oldest dropped first");
        // Last must be the (cap_pairs + 4) assistant message.
        let expected_last = format!("a{}", cap_pairs + 4);
        assert_eq!(h.last().unwrap().role, "assistant");
        assert_eq!(h.last().unwrap().content, expected_last);
    }

    // S04-AC4: prune drops entries older than TTL_SECS, keeps fresh.
    #[test]
    fn prune_drops_stale_keeps_fresh() {
        let mut store = SessionStore::default();
        let now = now_secs();
        // stale entry
        let c = store.chats.entry(1).or_default();
        c.aspects.insert(
            "stale".into(),
            AspectSession {
                history: vec![ChatMessage {
                    role: "user".into(),
                    content: "x".into(),
                }],
                last_active: now.saturating_sub(TTL_SECS + 60),
                generation: 0,
            },
        );
        // fresh entry
        c.aspects.insert(
            "fresh".into(),
            AspectSession {
                history: vec![],
                last_active: now,
                generation: 0,
            },
        );
        prune(&mut store);
        let c = store.chats.get(&1).unwrap();
        assert!(
            !c.aspects.contains_key("stale"),
            "stale entry should be pruned"
        );
        assert!(
            c.aspects.contains_key("fresh"),
            "fresh entry should survive"
        );
    }

    // S04-AC3: atomic write + flock — round-trip through disk yields identical state.
    #[test]
    fn save_load_roundtrip() {
        let (data, lock) = unique_paths("roundtrip");
        let mut store = SessionStore::default();
        store.set_sticky(99, "banshee");
        store.append_turn(99, "banshee", "the number is 42", "noted: 42");
        store.save_to(&data, &lock).unwrap();
        let loaded = SessionStore::load_from(&data, &lock).unwrap();
        assert_eq!(loaded.sticky(99), "banshee");
        let h = loaded.history(99, "banshee");
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].content, "the number is 42");
        assert_eq!(h[1].content, "noted: 42");
        // cleanup
        let _ = fs::remove_file(&data);
        let _ = fs::remove_file(&lock);
        let _ = fs::remove_dir(data.parent().unwrap());
    }

    // S04-AC5: malformed JSON on disk yields default state, no panic.
    #[test]
    fn malformed_json_yields_default() {
        let (data, lock) = unique_paths("malformed");
        ensure_dir(&data).unwrap();
        fs::write(&data, b"this is not json {{{").unwrap();
        let loaded = SessionStore::load_from(&data, &lock).unwrap();
        assert!(loaded.chats.is_empty());
        let _ = fs::remove_file(&data);
        let _ = fs::remove_file(&lock);
        let _ = fs::remove_dir(data.parent().unwrap());
    }

    #[test]
    fn missing_file_yields_default() {
        let (data, lock) = unique_paths("missing");
        // do not create data
        let loaded = SessionStore::load_from(&data, &lock).unwrap();
        assert!(loaded.chats.is_empty());
        let _ = fs::remove_file(&lock);
        let _ = fs::remove_dir(data.parent().unwrap());
    }
}
