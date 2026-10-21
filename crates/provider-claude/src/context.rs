//! What a host event says about the size of the root conversation and the prompt cache its last
//! request wrote. The Messages API reports both in each answer's `usage`: the context is
//! everything the request sent plus what it answered, and `cache_creation` splits the tokens it
//! cached by how long they last (platform.claude.com/docs/en/build-with-claude/prompt-caching).
use crate::adapter::host_event;
use alondra_agent_protocol::Observation as WireObservation;
use alondra_provider_contract::OrderedObservation;
use serde_json::Value;
use std::time::Duration;

const HOUR: Duration = Duration::from_secs(60 * 60);
const FIVE_MINUTES: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextEvent {
    /// Claude sends the model the next request of the root conversation: a tool's result goes
    /// back to it.
    Requested,
    /// `model` answered a request of the root conversation `tokens` long; `cache` is how long
    /// the prompt cache that request wrote lasts, unknown when it wrote none.
    Answered { tokens: u64, cache: Option<Duration>, model: Option<String> },
    /// A turn ended, and Claude said the context window of each model it used in it.
    Windows(Vec<(String, u64)>),
    /// Claude compacted the conversation: what was measured before no longer holds.
    Compacted,
}

pub fn context_event(observation: &OrderedObservation) -> Option<ContextEvent> {
    match host_event(observation)?.observation {
        WireObservation::Sdk { payload, .. } => of(&payload),
        _ => None,
    }
}

fn of(payload: &Value) -> Option<ContextEvent> {
    let root = payload.get("parent_tool_use_id").is_none_or(Value::is_null);
    match payload.get("type")?.as_str()? {
        "user" if root => Some(ContextEvent::Requested),
        "assistant" if root => {
            let usage = payload.pointer("/message/usage")?;
            let count = |field: &str| usage.get(field).and_then(Value::as_u64).unwrap_or(0);
            let cached = |field: &str| usage.get("cache_creation").and_then(|created| created.get(field)).and_then(Value::as_u64).unwrap_or(0) > 0;
            let tokens = count("input_tokens") + count("cache_creation_input_tokens") + count("cache_read_input_tokens") + count("output_tokens");
            let cache = if cached("ephemeral_1h_input_tokens") {
                Some(HOUR)
            } else if cached("ephemeral_5m_input_tokens") {
                Some(FIVE_MINUTES)
            } else {
                None
            };
            let model = payload.pointer("/message/model").and_then(Value::as_str).map(str::to_owned);
            Some(ContextEvent::Answered { tokens, cache, model })
        }
        "result" => {
            let windows: Vec<(String, u64)> = payload.get("modelUsage").and_then(Value::as_object).into_iter().flatten()
                .filter_map(|(model, usage)| Some((model.clone(), usage.get("contextWindow")?.as_u64().filter(|window| *window > 0)?)))
                .collect();
            (!windows.is_empty()).then_some(ContextEvent::Windows(windows))
        }
        "system" if payload.get("subtype").and_then(Value::as_str) == Some("compact_boundary") => Some(ContextEvent::Compacted),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn answer(usage: Value) -> Value {
        json!({"type": "assistant", "parent_tool_use_id": null, "message": {"role": "assistant", "content": [], "usage": usage}})
    }

    #[test]
    fn an_answer_measures_everything_its_request_sent_and_answered_and_how_long_its_cache_lasts() {
        let hour = answer(json!({"input_tokens": 3, "cache_creation_input_tokens": 1_000, "cache_read_input_tokens": 250_000, "output_tokens": 400,
            "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 1_000}}));
        assert_eq!(of(&hour), Some(ContextEvent::Answered { tokens: 251_403, cache: Some(HOUR), model: None }));
        let minutes = answer(json!({"input_tokens": 3, "cache_creation_input_tokens": 10, "cache_read_input_tokens": 0, "output_tokens": 1,
            "cache_creation": {"ephemeral_5m_input_tokens": 10, "ephemeral_1h_input_tokens": 0}}));
        assert_eq!(of(&minutes), Some(ContextEvent::Answered { tokens: 14, cache: Some(FIVE_MINUTES), model: None }));
    }

    #[test]
    fn an_answer_that_cached_nothing_says_nothing_of_when_the_cache_ends() {
        let read_only = answer(json!({"input_tokens": 3, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 300_000, "output_tokens": 1,
            "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 0}}));
        assert_eq!(of(&read_only), Some(ContextEvent::Answered { tokens: 300_004, cache: None, model: None }));
        let unsplit = answer(json!({"input_tokens": 1, "output_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}));
        assert_eq!(of(&unsplit), Some(ContextEvent::Answered { tokens: 2, cache: None, model: None }));
    }

    #[test]
    fn an_answer_names_its_model_and_a_turns_result_the_window_of_each_model() {
        let mut answered = answer(json!({"input_tokens": 1, "output_tokens": 1}));
        answered["message"]["model"] = json!("claude-opus-5-5");
        assert_eq!(of(&answered), Some(ContextEvent::Answered { tokens: 2, cache: None, model: Some("claude-opus-5-5".into()) }));
        let result = json!({"type": "result", "subtype": "success", "modelUsage": {
            "claude-opus-5-5": {"inputTokens": 9, "contextWindow": 1_000_000},
            "claude-haiku-4-5-20251001": {"inputTokens": 3, "contextWindow": 200_000},
            "unsized": {"inputTokens": 1}}});
        let Some(ContextEvent::Windows(mut windows)) = of(&result) else { panic!("a result with modelUsage says its windows") };
        windows.sort();
        assert_eq!(windows, vec![("claude-haiku-4-5-20251001".into(), 200_000), ("claude-opus-5-5".into(), 1_000_000)]);
    }

    #[test]
    fn a_subagents_messages_are_not_the_root_conversation() {
        let mut answered = answer(json!({"input_tokens": 1, "output_tokens": 1, "cache_creation": {"ephemeral_1h_input_tokens": 5}}));
        answered["parent_tool_use_id"] = json!("toolu_1");
        assert_eq!(of(&answered), None);
        assert_eq!(of(&json!({"type": "user", "parent_tool_use_id": "toolu_1", "message": {}})), None);
        assert_eq!(of(&json!({"type": "user", "parent_tool_use_id": null, "message": {}})), Some(ContextEvent::Requested));
    }

    #[test]
    fn a_compaction_and_other_events_say_what_they_say() {
        assert_eq!(of(&json!({"type": "system", "subtype": "compact_boundary", "compact_metadata": {"trigger": "manual", "pre_tokens": 250_000}})), Some(ContextEvent::Compacted));
        assert_eq!(of(&json!({"type": "system", "subtype": "status", "status": "compacting"})), None);
        assert_eq!(of(&json!({"type": "result", "subtype": "success", "usage": {"input_tokens": 900_000}})), None, "a turn's result adds up every request");
    }
}
