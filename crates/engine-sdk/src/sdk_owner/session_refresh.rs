//! A conversation resumed after its prompt cache expired is written whole to the cache again by its
//! first request, at twice the price of its input: four nodes coming back from a usage limit wrote
//! two million tokens in one burst. A large one starts instead as a new session of the same model
//! and account, from the task's records, and the conversation before it waits in a file the new
//! session reads only when it needs something of it.
//!
//! The decision is taken whenever a host whose session already holds a conversation is handed an
//! input: one that resumes a released, crashed or left session, one a restart adopted while it ran,
//! and one that stayed alive through a pause of hours. Only then is the time since the last request
//! known. A live host is ended by the refresh, as a move ends it.
use super::*;
use alondra_engine_ports::sdk::LastRequest;
use alondra_core::Millis;

/// The size from which a conversation whose prompt cache expired starts anew instead of resuming:
/// below it, rewriting the cache costs less than what a new session reads again to go on.
pub(crate) const FROM_TOKENS: u64 = 100_000;

/// What a host needs to find the conversation its session holds.
pub(super) struct Conversation {
    pub native_environment: BTreeMap<String, String>,
    pub directory: String,
}

/// Whether a session that would go on with a pending input starts anew instead: a Claude
/// conversation of at least `FROM_TOKENS` whose last request's cache, for as long as the provider
/// said it lasts, expired by `now`.
pub(crate) fn starts_anew(provider: Provider, last: Option<LastRequest>, now: Millis, pending_input: bool) -> bool {
    provider == Provider::Claude && pending_input
        && last.is_some_and(|last| last.tokens >= FROM_TOKENS && now >= last.requested_at.saturating_add(last.cache_ms))
}

/// Where the size and cache expiry a decision rests on came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// What this daemon kept of the task's last request.
    Stored,
    /// The last answer in Claude Code's transcript of the session, for a task whose last request
    /// was made before readings were kept, or whose reading was lost.
    Derived,
}

/// What the decision rests on, as the log says it.
fn basis(last: Option<LastRequest>, source: Option<Source>, absent: &str, now: Millis) -> String {
    let Some(last) = last else { return format!("no reading of its conversation ({absent})") };
    let source = match source {
        Some(Source::Derived) => "reading derived from the transcript",
        _ => "stored reading",
    };
    let expires_at = last.requested_at.saturating_add(last.cache_ms);
    let cache = if now >= expires_at {
        format!("cache expired {} min ago", (now - expires_at) / 60_000)
    } else {
        format!("cache holds {} more min", (expires_at - now + 59_999) / 60_000)
    };
    format!("{source}: {} tokens, {cache}", last.tokens)
}

/// The session `conversation` records and Claude Code's transcript of it.
fn located(domain: &dyn Runtime, task: &TaskId, stamp_conversation: &str, conversation: &Conversation) -> Result<(String, PathBuf), &'static str> {
    let metadata = domain.store().lock_or_exit("the domain store").conversation_metadata(task, stamp_conversation);
    let session = metadata.ok().flatten().as_deref().and_then(alondra_provider_claude::conversation_session).ok_or("its session is not recorded")?;
    let transcript = claude_history::transcript(&conversation.native_environment, &session, std::path::Path::new(&conversation.directory))
        .ok_or("Claude Code's transcript of the session was not found")?;
    Ok((session, transcript))
}

/// The last request of the root conversation in a Claude Code transcript, read from its last answer
/// with no model call: its size is what that answer's request sent (`input_tokens`,
/// `cache_read_input_tokens` and `cache_creation_input_tokens`), its time is that answer's, and its
/// cache lasts as long as the last cache an answer wrote (an hour, which nodes ask for, when none
/// says). An answer that used no tokens, as the ones Claude Code writes itself for an error, made
/// no request; a compaction after the last answer leaves nothing measured.
fn last_request_in(jsonl: &str) -> Option<LastRequest> {
    let mut answered: Option<(u64, Millis)> = None;
    let mut cache_ms = None;
    for line in jsonl.lines().rev() {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if record["isSidechain"].as_bool() == Some(true) {
            continue;
        }
        if record["type"] == "system" && record["subtype"] == "compact_boundary" {
            break;
        }
        if record["type"] != "assistant" {
            continue;
        }
        let usage = &record["message"]["usage"];
        let count = |field: &str| usage[field].as_u64().unwrap_or(0);
        let written = |field: &str| usage["cache_creation"][field].as_u64().unwrap_or(0);
        if answered.is_none() {
            let tokens = count("input_tokens") + count("cache_read_input_tokens") + count("cache_creation_input_tokens");
            if tokens == 0 {
                continue;
            }
            let at = record["timestamp"].as_str().and_then(crate::watcher::parse_rfc3339_millis)?;
            answered = Some((tokens, Millis::try_from(at).ok()?));
        }
        if written("ephemeral_1h_input_tokens") > 0 {
            cache_ms = Some(HOUR_MS);
        } else if written("ephemeral_5m_input_tokens") > 0 {
            cache_ms = Some(FIVE_MINUTES_MS);
        }
        if cache_ms.is_some() {
            break;
        }
    }
    let (tokens, requested_at) = answered?;
    Some(LastRequest { tokens, requested_at, cache_ms: cache_ms.unwrap_or(HOUR_MS) })
}

const HOUR_MS: Millis = 60 * 60_000;
const FIVE_MINUTES_MS: Millis = 5 * 60_000;

/// Decides, for the host of `task` in `host_directory` whose session holds `conversation`, whether
/// that session starts anew, and requests that new session when it does: the conversation file is
/// written and the refresh is committed, so the input waits for the new session. The size and
/// cache expiry come from the task's stored reading or, without one, from the session's transcript;
/// the log says which, and why the session starts anew or goes on. A refresh that cannot be
/// prepared leaves the session to go on, and says why. A session in the middle of a turn goes on,
/// and while a change of the task's session is under way nothing more is requested.
pub(super) fn refresh(domain: &dyn Runtime, task: &TaskId, provider: Provider, host_directory: &std::path::Path, conversation: &Conversation) -> bool {
    let (pending, stamp_conversation, instance) = {
        let read = domain.read();
        let Ok(lifecycle) = read.state().lifecycle_of(task) else { return false };
        let Some(stamp) = lifecycle.execution() else { return false };
        if lifecycle.turn().is_some() || matches!(lifecycle.activity(), ActivityView::Working | ActivityView::OwnWork) {
            return false;
        }
        (!lifecycle.pending_inputs().is_empty(), stamp.conversation().as_str().to_owned(), stamp.instance().as_str().to_owned())
    };
    if provider != Provider::Claude || !pending {
        return false;
    }
    let now = alondra_core::now_millis();
    let mut found = None;
    let (last, source, absent) = match domain.engine().last_request(task) {
        Some(last) => (Some(last), Some(Source::Stored), ""),
        None => {
            let derived = located(domain, task, &stamp_conversation, conversation).and_then(|(session, transcript)| {
                let last = std::fs::read_to_string(&transcript).ok().as_deref().and_then(last_request_in);
                found = Some((session, transcript));
                last.ok_or("the transcript holds no timed answer since its last compaction")
            });
            match derived {
                Ok(last) => (Some(last), Some(Source::Derived), ""),
                Err(why) => (None, None, why),
            }
        }
    };
    let basis = basis(last, source, absent, now);
    if !starts_anew(provider, last, now, pending) {
        log!("{task} resumes its conversation instead of starting a new session; {basis}");
        return false;
    }
    let resumes = |why: &str| {
        log!("{task} resumes its conversation, whose prompt cache expired, instead of starting a new session: {why}; {basis}");
        false
    };
    let (open, latest) = {
        let store = domain.store().lock_or_exit("the domain store");
        (store.provider_transition(task), store.latest_provider_transition(task))
    };
    if !matches!(open, Ok(None)) {
        log!("{task} resumes its conversation while a change of its session is under way; {basis}");
        return false;
    }
    if let Ok(Some(change)) = latest
        && change.plan.kind == crate::domain_store::provider_transition::Kind::Refresh
        && change.plan.source.instance == instance
        && change.phase == crate::domain_store::provider_transition::Phase::Aborted {
        return resumes(&format!("a new session was already tried for it and did not start ({})", change.facts["reason"].as_str().unwrap_or("no reason kept")));
    }
    let (session, transcript) = match found.map_or_else(|| located(domain, task, &stamp_conversation, conversation), Ok) {
        Ok(found) => found,
        Err(why) => return resumes(why),
    };
    let file = match write_conversation(domain.home(), task, &session, &transcript) {
        Ok(file) => file,
        Err(error) => return resumes(&format!("the conversation file could not be written ({error})")),
    };
    let scope = match AuthorityScope::new(domain.read().state().data_epoch.clone()) {
        Ok(scope) => scope,
        Err(error) => return resumes(&format!("{error:?}")),
    };
    match domain.own(scope, |owner| crate::provider_transition::request_refresh(owner, task, host_directory.to_path_buf(), file.clone())) {
        Ok((id, _)) => {
            log!("{task} starts a new session ({id}) instead of resuming its conversation, whose prompt cache expired; {basis}; the conversation is in {}", file.display());
            true
        }
        Err(error) => resumes(&format!("the handoff could not be prepared ({})", store_text(&error))),
    }
}

/// Writes the conversation of `session`, condensed from its `transcript`, in the task's own folder
/// of Alondra's files, which goes when the task closes.
fn write_conversation(home: &std::path::Path, task: &TaskId, session: &str, transcript: &std::path::Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let folder = crate::files_retention::task_scratch(home, &task.0)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "the task's id names no folder"))?;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&folder)?;
    let jsonl = std::fs::read_to_string(transcript)?;
    let text = super::conversation_file::condensed(&jsonl, transcript, &claude_history::subagent_transcripts(transcript));
    let file = folder.join(format!("conversation-{session}.md"));
    alondra_foundation::save::atomic(&file, text.as_bytes())?;
    Ok(file)
}

#[cfg(all(test, not(feature = "whole-engine")))]
mod tests {
    use super::*;

    const HOUR: Millis = 60 * 60_000;

    fn last(tokens: u64) -> Option<LastRequest> {
        Some(LastRequest { tokens, requested_at: 10 * HOUR, cache_ms: HOUR })
    }

    #[test]
    fn only_a_large_claude_conversation_whose_cache_expired_and_that_has_work_starts_anew() {
        let expired = 11 * HOUR;
        assert!(starts_anew(Provider::Claude, last(250_000), expired, true));
        assert!(starts_anew(Provider::Claude, last(FROM_TOKENS), expired + 4 * HOUR, true), "from the threshold on");
        assert!(!starts_anew(Provider::Claude, last(FROM_TOKENS - 1), expired, true), "a small conversation is cheaper to resume");
        assert!(!starts_anew(Provider::Claude, last(250_000), expired - 1, true), "the cache still holds it");
        assert!(!starts_anew(Provider::Claude, None, expired, true), "nothing timed, nothing known to have expired");
        assert!(!starts_anew(Provider::Claude, last(250_000), expired, false), "nothing to go on with");
        assert!(!starts_anew(Provider::Codex, last(250_000), expired, true), "another provider caches otherwise");
        let five_minutes = Some(LastRequest { cache_ms: 5 * 60_000, ..last(250_000).unwrap() });
        assert!(starts_anew(Provider::Claude, five_minutes, 10 * HOUR + 5 * 60_000, true), "the expiry the provider gave decides");
    }

    fn answer(at: &str, usage: serde_json::Value) -> String {
        serde_json::json!({"type": "assistant", "isSidechain": false, "timestamp": at, "message": {"role": "assistant", "usage": usage}}).to_string()
    }

    fn wrote(hour: u64, minutes: u64) -> serde_json::Value {
        serde_json::json!({"input_tokens": 3, "cache_read_input_tokens": 248_000, "cache_creation_input_tokens": hour + minutes, "output_tokens": 400,
            "cache_creation": {"ephemeral_1h_input_tokens": hour, "ephemeral_5m_input_tokens": minutes}})
    }

    const AT: &str = "2026-11-20T07:38:00.000Z";
    const AT_MS: Millis = 1_795_160_280_000;

    #[test]
    fn a_transcripts_last_answer_says_how_large_the_conversation_is_and_when_its_cache_expires() {
        let user = serde_json::json!({"type": "user", "timestamp": "2026-11-20T07:39:00.000Z", "message": {"role": "user", "content": "go on"}}).to_string();
        let jsonl = [answer("2026-11-20T07:00:00.000Z", wrote(0, 9)), answer(AT, wrote(2_000, 0)), user].join("\n");
        assert_eq!(last_request_in(&jsonl), Some(LastRequest { tokens: 250_003, requested_at: AT_MS, cache_ms: HOUR }));
        let minutes = answer(AT, wrote(0, 2_000));
        assert_eq!(last_request_in(&minutes).map(|last| last.cache_ms), Some(5 * 60_000));
    }

    #[test]
    fn an_answer_that_wrote_no_cache_lasts_as_long_as_the_last_cache_written() {
        let read_only = serde_json::json!({"input_tokens": 3, "cache_read_input_tokens": 250_000, "cache_creation_input_tokens": 0, "output_tokens": 1});
        let jsonl = [answer("2026-11-20T07:00:00.000Z", wrote(0, 9)), answer(AT, read_only.clone())].join("\n");
        assert_eq!(last_request_in(&jsonl), Some(LastRequest { tokens: 250_003, requested_at: AT_MS, cache_ms: 5 * 60_000 }));
        assert_eq!(last_request_in(&answer(AT, read_only)).map(|last| last.cache_ms), Some(HOUR), "nodes ask for an hour");
    }

    #[test]
    fn errors_subagents_and_compactions_are_not_the_last_request() {
        let error = answer("2026-11-20T09:00:00.000Z", serde_json::json!({"input_tokens": 0, "output_tokens": 0}));
        let mut subagent: serde_json::Value = serde_json::from_str(&answer("2026-11-20T09:01:00.000Z", wrote(5, 0))).unwrap();
        subagent["isSidechain"] = true.into();
        let jsonl = [answer(AT, wrote(2_000, 0)), error, subagent.to_string(), "not json".into()].join("\n");
        assert_eq!(last_request_in(&jsonl).map(|last| (last.tokens, last.requested_at)), Some((250_003, AT_MS)));
        let compacted = [answer(AT, wrote(2_000, 0)), serde_json::json!({"type": "system", "subtype": "compact_boundary"}).to_string()].join("\n");
        assert_eq!(last_request_in(&compacted), None);
        assert_eq!(last_request_in(""), None);
    }

    #[test]
    fn the_log_says_what_the_decision_rests_on() {
        let last = Some(LastRequest { tokens: 155_628, requested_at: 10 * HOUR, cache_ms: HOUR });
        assert_eq!(basis(last, Some(Source::Derived), "", 14 * HOUR), "reading derived from the transcript: 155628 tokens, cache expired 180 min ago");
        assert_eq!(basis(last, Some(Source::Stored), "", 10 * HOUR + 30 * 60_000), "stored reading: 155628 tokens, cache holds 30 more min");
        assert_eq!(basis(None, None, "its session is not recorded", 0), "no reading of its conversation (its session is not recorded)");
    }
}
