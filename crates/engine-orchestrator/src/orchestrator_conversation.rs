//! The orchestrator keeps one conversation: every batch is a new turn of it, run by a host of its
//! own that resumes it, so each turn keeps its own execution, capability and guarantees while the
//! conversation, and the prompt cache with it, carries on.
//!
//! A conversation grows with every turn, and one that was resumed without limit grew to half a
//! million tokens a batch. So at the start of a turn, and only then, Alondra decides whether the
//! conversation goes on or a new one starts, on facts alone:
//!
//! - there is none yet, or the last turn that resumed it never started (its session is gone);
//! - the turn runs on another provider or account than the conversation was held by;
//! - the conversation reached `MIGRATE_TOKENS`;
//! - Claude's prompt cache of its last request expired and it is at least
//!   `REFRESH_FROM_TOKENS`, the rule a node's conversation follows: resuming would write all of it
//!   to the cache again.
//!
//! A new conversation starts from a handoff no model writes: what the domain holds now, what the
//! orchestrator still owes, the end of the old conversation's last answer, and the old
//! conversation itself, condensed into a file in the orchestrator's working directory that the new
//! one searches only when it needs something of it.
use alondra_core::Millis;
use alondra_core::accounts::{AccountKey, Provider};
use alondra_engine_ports::sdk::REFRESH_FROM_TOKENS;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The size at which the conversation starts anew even while its prompt cache holds.
pub const MIGRATE_TOKENS: u64 = 150_000;
/// How long Claude keeps the prompt cache of an orchestrator request: the host pins it to an hour.
pub const CLAUDE_CACHE_MS: Millis = 60 * 60_000;
/// The most of the old conversation's last answer a handoff carries.
const LAST_WORDS_BYTES: usize = 12 * 1024;
/// How many condensed conversations are kept beside the one in use.
const KEPT_CONVERSATIONS: usize = 20;
/// Where the condensed conversations are kept, inside the orchestrator's working directory.
const FOLDER: &str = "conversations";

/// The conversation the orchestrator's turns resume.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Conversation {
    pub id: String,
    pub provider: Provider,
    pub account: String,
    /// Its turns that ended with their result.
    pub turns: u64,
    /// Its size when its last turn ended: what the model read in that turn's last call.
    pub tokens: u64,
    /// When its last turn ended, and how long the prompt cache of that turn lasts.
    pub last_request_at: Option<Millis>,
    pub cache_ms: Millis,
    pub started_at: Millis,
    pub started_because: Migration,
    /// The end of its last answer, for the handoff of the conversation after it.
    #[serde(default)]
    pub last_words: Option<String>,
    /// A turn that resumed it did not start: its session cannot be resumed any more.
    #[serde(default)]
    pub resume_failed: bool,
    /// What it was told of the conversation before it, in its first turn, kept until a turn of it
    /// ends: a conversation whose first turn never ended starts again with the same words.
    #[serde(default)]
    pub opening: Option<String>,
}

/// Why a conversation started.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Migration {
    /// There was none before it.
    First,
    /// The one before reached `MIGRATE_TOKENS`.
    Size,
    /// The prompt cache of the one before expired and it was at least `REFRESH_FROM_TOKENS`.
    CacheExpired,
    /// The turn runs on another account or provider than the one before.
    Account,
    /// A turn that resumed the one before did not start.
    Lost,
}

impl Migration {
    pub fn text(self) -> &'static str {
        match self {
            Self::First => "there was no conversation yet",
            Self::Size => "the conversation reached its size limit",
            Self::CacheExpired => "its prompt cache expired with a large conversation",
            Self::Account => "the turn runs on another account or provider",
            Self::Lost => "the conversation could not be resumed",
        }
    }
}

/// Whether a turn on `account` at `now` starts a new conversation instead of resuming `current`,
/// and why. Codex reports no prompt cache, so only size, account and a lost session move it.
pub fn migration(current: Option<&Conversation>, account: &AccountKey, now: Millis) -> Option<Migration> {
    let Some(current) = current else { return Some(Migration::First) };
    if current.turns == 0 {
        return Some(current.started_because);
    }
    if current.resume_failed {
        return Some(Migration::Lost);
    }
    if current.provider != account.provider || current.account != account.account.to_string() {
        return Some(Migration::Account);
    }
    if current.tokens >= MIGRATE_TOKENS {
        return Some(Migration::Size);
    }
    let expired = current.provider == Provider::Claude && current.tokens >= REFRESH_FROM_TOKENS
        && current.last_request_at.is_some_and(|at| now >= at.saturating_add(current.cache_ms));
    expired.then_some(Migration::CacheExpired)
}

impl Conversation {
    pub fn new(id: String, account: &AccountKey, now: Millis, started_because: Migration) -> Self {
        Self {
            id, provider: account.provider, account: account.account.to_string(), turns: 0, tokens: 0, last_request_at: None,
            cache_ms: if account.provider == Provider::Claude { CLAUDE_CACHE_MS } else { 0 }, started_at: now, started_because,
            last_words: None, resume_failed: false, opening: None,
        }
    }

    /// A turn of it read `tokens` in its last call, ending at `now`, however the turn ended: the
    /// conversation is that large now, and its cache counts from then.
    pub fn read(&mut self, tokens: u64, now: Millis) {
        self.tokens = tokens;
        self.last_request_at = Some(now);
    }

    /// A turn of it ended with its result, `last_words` the end of its answer.
    pub fn said(&mut self, last_words: Option<&str>) {
        self.last_words = last_words.map(|words| tail(words, LAST_WORDS_BYTES)).filter(|words| !words.is_empty());
    }

    /// A batch that ran a turn of it was handled, also one a restart found handled: it can be
    /// resumed, and what its first turn was told is no longer needed.
    pub fn turn_handled(&mut self) {
        self.turns += 1;
        self.opening = None;
    }

    /// A turn of it ended with its result at `now`, the model reading `tokens` in its last call.
    #[cfg(test)]
    pub fn turn_ended(&mut self, tokens: Option<u64>, last_words: Option<&str>, now: Millis) {
        if let Some(tokens) = tokens {
            self.read(tokens, now);
        } else {
            self.last_request_at = Some(now);
        }
        self.said(last_words);
        self.turn_handled();
    }
}

/// The end of `text`, at most `bytes` long, cut at a character boundary.
fn tail(text: &str, bytes: usize) -> String {
    let text = text.trim();
    if text.len() <= bytes {
        return text.to_owned();
    }
    let mut start = text.len() - bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &text[start..])
}

/// Where the condensed text of conversation `id` is kept, in the orchestrator's working directory.
pub fn conversation_file(directory: &Path, id: &str) -> PathBuf {
    directory.join(FOLDER).join(format!("conversation-{id}.md"))
}

/// Writes `transcript` of conversation `id` condensed beside the orchestrator, and keeps only the
/// newest `KEPT_CONVERSATIONS`.
pub fn write_conversation(directory: &Path, id: &str, transcript: &Path) -> std::io::Result<PathBuf> {
    let jsonl = std::fs::read_to_string(transcript)?;
    let subagents = alondra_engine_ports::claude_history::subagent_transcripts(transcript);
    let text = alondra_engine_ports::conversation_file::condensed(&jsonl, transcript, &subagents);
    let path = conversation_file(directory, id);
    // What users asked is in it: the folder and the file are the user's alone.
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path.parent().expect("the file has a folder"))?;
    }
    let next = path.with_extension("md.next");
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&next)?;
        file.write_all(text.as_bytes())?;
    }
    std::fs::rename(&next, &path)?;
    prune(&directory.join(FOLDER));
    Ok(path)
}

fn prune(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries.filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".md"))
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    files.sort();
    let surplus = files.len().saturating_sub(KEPT_CONVERSATIONS);
    for (_, path) in files.into_iter().take(surplus) {
        let _ = std::fs::remove_file(path);
    }
}

/// What a new conversation is told of the one before it. `file` is the condensed old conversation
/// when it could be written.
pub fn handoff(previous: &Conversation, why: Migration, file: Option<&Path>) -> String {
    let mut text = format!(
        "\nTHIS IS A NEW CONVERSATION. Alondra started it because {}; the conversation before it ({} turns) is over. Everything durable is \
         below or in the domain: the state Alondra writes here is current, and what you still owe is listed in it. Nothing has to be \
         done again because the conversation changed.\n",
        why.text(), previous.turns);
    match file {
        Some(file) => text.push_str(&format!(
            "The old conversation, condensed, is in {}: an index of its turns, then each turn under \"{}<n>\" with what was said and one \
             line per tool call. Search it with Grep or read a part of it with Read only when you need something of it; never read it whole.\n",
            file.display(), alondra_engine_ports::conversation_file::TURN)),
        None => text.push_str("The old conversation could not be kept in a file: what the domain holds is all there is of it.\n"),
    }
    if let Some(words) = &previous.last_words {
        text.push_str(&format!("The end of your last answer in it:\n{words}\n"));
    }
    text.push_str("MEMORY: none yet.\n");
    text
}

#[cfg(test)]
#[cfg(not(feature = "real-hosts"))]
mod tests {
    use super::*;
    use alondra_core::accounts::AccountId;

    const HOUR: Millis = 60 * 60_000;

    fn claude() -> AccountKey {
        AccountKey::new(Provider::Claude, AccountId::default_account())
    }

    fn held(tokens: u64, last_request_at: Millis) -> Conversation {
        let mut conversation = Conversation::new("c-1".into(), &claude(), 0, Migration::First);
        conversation.turn_ended(Some(tokens), Some("Done."), last_request_at);
        conversation
    }

    #[test]
    fn the_first_turn_starts_a_conversation_and_the_next_resumes_it() {
        assert_eq!(migration(None, &claude(), 0), Some(Migration::First));
        let unstarted = Conversation::new("c-0".into(), &claude(), 0, Migration::Size);
        assert_eq!(migration(Some(&unstarted), &claude(), 1), Some(Migration::Size), "one whose first turn never ended starts again, for the same reason");
        assert_eq!(migration(Some(&held(40_000, 0)), &claude(), 5 * 60_000), None, "a small conversation with its cache resumes");
    }

    #[test]
    fn a_conversation_moves_at_its_size_limit_even_while_its_cache_holds() {
        assert_eq!(migration(Some(&held(MIGRATE_TOKENS - 1, 0)), &claude(), 1), None);
        assert_eq!(migration(Some(&held(MIGRATE_TOKENS, 0)), &claude(), 1), Some(Migration::Size));
    }

    #[test]
    fn an_expired_cache_moves_only_a_large_conversation_and_only_once_it_expired() {
        let large = held(REFRESH_FROM_TOKENS, 0);
        assert_eq!(migration(Some(&large), &claude(), HOUR - 1), None, "the cache still holds");
        assert_eq!(migration(Some(&large), &claude(), HOUR), Some(Migration::CacheExpired));
        assert_eq!(migration(Some(&held(REFRESH_FROM_TOKENS - 1, 0)), &claude(), 24 * HOUR), None,
            "a small one resumes: rewriting its cache costs less than starting over");
    }

    #[test]
    fn another_account_or_provider_or_a_lost_session_moves_it() {
        let conversation = held(10_000, 0);
        assert_eq!(migration(Some(&conversation), &AccountKey::new(Provider::Claude, AccountId::parse("work").unwrap()), 1), Some(Migration::Account));
        assert_eq!(migration(Some(&conversation), &AccountKey::new(Provider::Codex, AccountId::default_account()), 1), Some(Migration::Account));
        let mut lost = conversation;
        lost.resume_failed = true;
        assert_eq!(migration(Some(&lost), &claude(), 1), Some(Migration::Lost));
    }

    #[test]
    fn codex_moves_on_size_and_account_but_has_no_cache_to_expire() {
        let codex = AccountKey::new(Provider::Codex, AccountId::default_account());
        let mut conversation = Conversation::new("c-1".into(), &codex, 0, Migration::First);
        conversation.turn_ended(Some(REFRESH_FROM_TOKENS + 1), None, 0);
        assert_eq!(migration(Some(&conversation), &codex, 24 * HOUR), None);
        conversation.turn_ended(Some(MIGRATE_TOKENS), None, 0);
        assert_eq!(migration(Some(&conversation), &codex, 1), Some(Migration::Size));
    }

    #[test]
    fn the_handoff_names_why_the_old_file_and_the_last_words_and_keeps_room_for_memory() {
        let mut old = held(MIGRATE_TOKENS, 0);
        old.last_words = Some("Created the three phase tasks.".into());
        let text = handoff(&old, Migration::Size, Some(Path::new("/o/conversations/conversation-c-1.md")));
        assert!(text.contains("reached its size limit") && text.contains("/o/conversations/conversation-c-1.md")
            && text.contains("## Turn <n>") && text.contains("Created the three phase tasks.") && text.contains("MEMORY: none yet."), "{text}");
        assert!(handoff(&old, Migration::Lost, None).contains("could not be kept in a file"));
    }

    #[test]
    fn the_last_words_keep_their_end() {
        let mut conversation = held(1, 0);
        let long = format!("{}THE END", "x".repeat(LAST_WORDS_BYTES * 2));
        conversation.turn_ended(None, Some(&long), 1);
        let words = conversation.last_words.unwrap();
        assert!(words.ends_with("THE END") && words.len() <= LAST_WORDS_BYTES + "…".len());
        assert_eq!(conversation.tokens, 1, "a turn with no reading keeps the size it had");
    }
}
