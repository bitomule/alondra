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
use crate::idle_compaction::LastRequest;
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

/// Decides, for the host of `task` in `host_directory` whose session holds `conversation`, whether
/// that session starts anew, and requests that new session when it does: the conversation file is
/// written and the refresh is committed, so the input waits for the new session. A refresh that
/// cannot be prepared leaves the session to go on, and says why. A session in the middle of a turn
/// goes on, and while a change of the task's session is under way nothing more is requested.
pub(super) fn refresh(domain: &DomainRuntime, task: &TaskId, provider: Provider, host_directory: &std::path::Path, conversation: &Conversation) -> bool {
    let (pending, stamp_conversation, instance) = {
        let read = domain.read();
        let Ok(lifecycle) = read.state().lifecycle_of(task) else { return false };
        let Some(stamp) = lifecycle.execution() else { return false };
        if lifecycle.turn().is_some() || matches!(lifecycle.activity(), ActivityView::Working | ActivityView::OwnWork) {
            return false;
        }
        (!lifecycle.pending_inputs().is_empty(), stamp.conversation().as_str().to_owned(), stamp.instance().as_str().to_owned())
    };
    let last = domain.contexts().last_request(task);
    if !starts_anew(provider, last, alondra_core::now_millis(), pending) {
        return false;
    }
    let tokens = last.map_or(0, |last| last.tokens);
    let resumes = |why: &str| {
        log!("{task} resumes its conversation of {tokens} tokens, whose prompt cache expired, instead of starting a new session: {why}");
        false
    };
    let (open, latest, metadata) = {
        let store = domain.store.lock_or_exit("the domain store");
        (store.provider_transition(task), store.latest_provider_transition(task), store.conversation_metadata(task, &stamp_conversation))
    };
    if !matches!(open, Ok(None)) {
        return false;
    }
    if let Ok(Some(change)) = latest
        && change.plan.kind == crate::domain_store::provider_transition::Kind::Refresh
        && change.plan.source.instance == instance
        && change.phase == crate::domain_store::provider_transition::Phase::Aborted {
        return resumes(&format!("a new session was already tried for it and did not start ({})", change.facts["reason"].as_str().unwrap_or("no reason kept")));
    }
    let Some(session) = metadata.ok().flatten().as_deref().and_then(alondra_provider_claude::conversation_session) else {
        return resumes("its session is not recorded");
    };
    let Some(transcript) = claude_history::transcript(&conversation.native_environment, &session, std::path::Path::new(&conversation.directory)) else {
        return resumes("Claude Code's transcript of the session was not found");
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
            log!("{task} starts a new session ({id}) instead of resuming its conversation of {tokens} tokens, whose prompt cache expired; the conversation is in {}", file.display());
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

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Millis = 60 * 60_000;

    fn last(tokens: u64) -> Option<LastRequest> {
        Some(LastRequest { tokens, requested_at: 10 * HOUR, cache_ms: HOUR, compactable: false })
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
}
