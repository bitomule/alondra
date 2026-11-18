//! How a message's delivery reads: the words the TUI and the commands show for its state.
use alondra_core::delivery::State;
use serde_json::Value;

pub fn state_text(state: State) -> &'static str {
    match state {
        State::Dispatching => tr!(
            "Dispatching: outcome pending; do not repeat",
            "En curso: resultado pendiente; no repitas"
        ),
        State::Uncertain => tr!(
            "Uncertain: check what it was about before deciding",
            "Incierto: revisa de qué trataba antes de decidir"
        ),
        State::Handed => tr!(
            "Handed to Claude; execution not confirmed",
            "Entregado a Claude; ejecución no confirmada"
        ),
        State::Retryable => tr!(
            "Retry permitted; execution not confirmed",
            "Reintento permitido; ejecución no confirmada"
        ),
        State::Discarded => tr!(
            "Discarded; previous effects are not undone",
            "Descartado; no deshace efectos anteriores"
        ),
    }
}
/// Used by every message consumer rather than treating HTTP 200 as completed execution.
pub fn response_notice(value: &Value, mac: Option<&str>) -> Option<String> {
    let raw = value.get("delivery_state")?;
    let text = serde_json::from_value::<State>(raw.clone())
        .map(state_text)
        .unwrap_or(tr!(
            "Unknown delivery outcome; inspect before repeating",
            "Resultado desconocido; revisa antes de repetir"
        ));
    Some(format!(
        "{} · {} list / :deliveries",
        text,
        command_prefix(mac)
    ))
}
/// `alondra delivery`, for the Mac `mac` names when it is not this one.
pub fn command_prefix(mac: Option<&str>) -> String {
    match mac {
        Some(mac) => format!("alondra delivery --mac {mac}"),
        None => "alondra delivery".into(),
    }
}
