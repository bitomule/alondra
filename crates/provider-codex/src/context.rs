use crate::adapter::host_event;
use alondra_codex_protocol::Observation;
use alondra_provider_contract::OrderedObservation;
use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextEvent {
    Requested,
    Answered { tokens: u64, cache: Option<Duration>, model: Option<String> },
    Windows(Vec<(String, u64)>),
    Compacted,
}
pub fn native_message(observation: &OrderedObservation) -> Option<Value> {
    match host_event(observation)?.observation { Observation::Sdk { payload, .. } => Some(payload), _ => None }
}
pub fn context_event(observation: &OrderedObservation) -> Option<ContextEvent> {
    let message = native_message(observation)?;
    match message.get("method")?.as_str()? {
        "alondra/inputAccepted" => Some(ContextEvent::Requested),
        "thread/tokenUsage/updated" => {
            let usage = message.pointer("/params/tokenUsage/last")?;
            Some(ContextEvent::Answered { tokens: usage.get("totalTokens")?.as_u64()?, cache: None, model: None })
        }
        "thread/compacted" => Some(ContextEvent::Compacted),
        "item/completed" if message.pointer("/params/item/type").and_then(Value::as_str) == Some("contextCompaction") => Some(ContextEvent::Compacted),
        _ => None,
    }
}

/// A positive local guard rejection: no `turn/start` was sent for this input.
pub fn account_refusal(observation:&OrderedObservation)->Option<&'static str>{
    let message=native_message(observation)?;
    match message.get("method")?.as_str()?{"alondra/accountBlocked"=>{},"alondra/inputRefused" if message.pointer("/params/nativeSubmitted").and_then(Value::as_bool)==Some(false)=>{},_=>return None}
    Some(match message.pointer("/params/code")?.as_str()?{
        "paid_authorization_required"=>"Codex now uses an API key without this node's original paid authorization. Create a new node and explicitly choose its paid account",
        "native_account_identity_changed"=>"This Codex profile now has a different native account or workspace. Restore the original identity or create a new node; this session will not switch accounts",
        "native_api_key_resume_unverifiable"=>"The original API key cannot be identified safely in a new Codex process. Create a new node with an explicit paid account selection",
        "native_account_identity_unavailable"=>"The original Codex account identity is unavailable. Create a new node; existing native history will not be attached to an unverified account",
        "native_auth_unavailable"=>"Codex account metadata could not be verified before starting the turn. No native input was submitted",
        "native_model_provider_unsupported"|"native_provider_auth_unsupported"|"native_auth_mode_unsupported"=>"This native authentication or model-provider mode is not supported safely by Alondra. Use a managed ChatGPT account, or explicitly choose an API-key account for a new node",
        "native_auth_policy_invalid"|"native_account_identity_invalid"=>"The private Codex account policy could not be verified. No native input was submitted",
        _=>return None,
    })
}
