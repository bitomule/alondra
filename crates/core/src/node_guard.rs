//! What a node may never do, whichever path its tool call takes: the daemon's hook for a
//! `claude --bg` node, the CLI's own hook and the domain's decision for a node under the SDK.
use serde_json::Value;

pub const USER_KEY_FILE: &str = "tui-key.pem";
pub const USER_ONLY_COMMANDS: &[&str] = &["answer", "send", "device", "orchestrator"];

/// Nodes share the user's account, so they could run the CLI that signs as the user. The
/// plugin's hook refuses it; a determined node could still hide the command, which is why
/// the phone and the Mac app keep their keys in the Secure Enclave.
pub fn forbidden_for_nodes(tool: Option<&str>, input: Option<&Value>) -> Option<String> {
    let field = |k: &str| input.and_then(|i| i.get(k)).and_then(Value::as_str).unwrap_or("");
    let touches_key = |s: &str| s.contains(USER_KEY_FILE) || (s.contains("Application Support") && s.to_lowercase().contains("alondra"));
    match tool? {
        "Bash" => {
            let command = field("command");
            let words: Vec<&str> = command.split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '`')).filter(|w| !w.is_empty()).collect();
            // Global flags such as --no-input may come before the command.
            let runs_user_command = words.iter().enumerate()
                .filter(|(_, w)| **w == "alondra" || w.ends_with("/alondra"))
                .filter_map(|(at, _)| words[at + 1..].iter().find(|w| !w.starts_with('-')))
                .any(|command| USER_ONLY_COMMANDS.contains(command));
            if runs_user_command {
                return Some("Nodes cannot run `alondra answer`, `send`, `device` or `orchestrator`: they act as the user. Use your Alondra MCP tools (ask, present_result) instead.".into());
            }
            let opens_home_database = words.iter().any(|w| *w == "sqlite3" || w.ends_with("/sqlite3"))
                && words.iter().any(|w| w.contains(".alondra/"))
                && !words.iter().any(|w| *w == "-readonly" || *w == "--readonly" || w.contains("mode=ro"));
            if opens_home_database {
                return Some("Open Alondra's databases with `sqlite3 -readonly`: a plain `sqlite3` creates the file when it is missing, and an empty alondra.db stops the daemon from starting.".into());
            }
            touches_key(command).then(|| "Nodes cannot touch the user's signing key.".into())
        }
        _ => ["file_path", "path", "pattern", "notebook_path"]
            .iter()
            .any(|k| touches_key(field(k)))
            .then(|| "Nodes cannot touch the user's signing key.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bash(c: &str) -> Option<String> {
        forbidden_for_nodes(Some("Bash"), Some(&json!({"command": c})))
    }

    #[test]
    fn nodes_cannot_answer_as_the_user() {
        assert!(bash("alondra answer 55 Mergea").is_some());
        assert!(bash("cd /tmp && ~/.local/bin/alondra send orquestador hola").is_some());
        assert!(bash("cat ~/.alondra/tui-key.pem").is_some());
        assert!(bash("ls ~/Library/'Application Support'/com.bitomule.alondra.mac").is_some());
        assert!(forbidden_for_nodes(Some("Read"), Some(&json!({"file_path": "/Users/x/.alondra/tui-key.pem"}))).is_some());
    }

    #[test]
    fn nodes_open_alondra_databases_only_read_only() {
        let refused = bash("sqlite3 ~/.alondra/alondra.db \".tables\" 2>/dev/null | head").unwrap();
        assert!(refused.contains("sqlite3 -readonly"), "{refused}");
        assert!(bash("cd /tmp; /usr/bin/sqlite3 /Users/x/.alondra/domain.db 'select 1'").is_some());
        assert!(bash("sqlite3 -readonly ~/.alondra/alondra.db .tables").is_none());
        assert!(bash("sqlite3 'file:/Users/x/.alondra/domain.db?mode=ro' .tables").is_none());
        assert!(bash("sqlite3 /tmp/copy.db .tables").is_none());
    }

    #[test]
    fn ordinary_commands_pass() {
        assert!(bash("alondra status").is_none());
        assert!(bash("alondra doctor --json").is_none());
        assert!(bash("git commit -m 'answer the question'").is_none());
        assert!(forbidden_for_nodes(Some("Read"), Some(&json!({"file_path": "/tmp/x.md"}))).is_none());
    }
}
