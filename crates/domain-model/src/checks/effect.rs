//! A command that answers Ok changed something, and says what. One that would change nothing is
//! refused with `no_effect`, so a caller never reads a confirmation that nothing backs. A repeated
//! command (same command id) is answered by the store with its first outcome before it gets here.
use super::*;




/// The command's name as its callers send it.
pub fn command_name(command: &Command) -> String {
    serde_json::to_value(command).ok()
        .and_then(|value| value.get("kind").and_then(serde_json::Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| "command".into())
}

