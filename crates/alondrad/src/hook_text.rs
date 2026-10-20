use alondra_core::{QuestionOption, answer_option};
use serde_json::Value;

pub(crate) fn str_field(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

pub(crate) fn tool_summary(tool: Option<&str>, input: Option<&Value>) -> Option<String> {
    let input = input?;
    let pick = |k: &str| input.get(k).and_then(Value::as_str).map(str::to_string);
    let s = match tool {
        Some("AskUserQuestion") => input
            .get("questions")
            .and_then(|q| q.get(0))
            .and_then(|q| q.get("question"))
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => pick("command").or_else(|| pick("file_path")).or_else(|| pick("description")).or_else(|| pick("pattern")).or_else(|| pick("url")),
    }?;
    Some(s.chars().take(300).collect())
}

/// The task one of Alondra's own tools names in its `task` argument.
pub(crate) fn named_task(tool: Option<&str>, input: Option<&Value>) -> Option<String> {
    tool.filter(|t| t.starts_with(alondra_core::activity::ALONDRA_TOOLS))?;
    input?.get("task")?.as_str().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string)
}

/// Keep enough context to explain lifecycle hooks without storing arbitrary tool input or
/// streaming message text. The source event name and task IDs are stored separately.
pub(crate) fn hook_detail(event: &str, input: &Value) -> Option<String> {
    let fields: &[&str] = match event {
        "SessionStart" | "SessionEnd" | "PreCompact" | "PostCompact" => &["reason", "source", "trigger"],
        "TaskCreated" | "TaskCompleted" => &["task_id", "task_subject", "task_description"],
        "PermissionDenied" => &["tool_name", "permission_mode", "reason"],
        "ConfigChange" | "InstructionsLoaded" => &["source", "file_path", "reason"],
        "CwdChanged" | "DirectoryAdded" | "FileChanged" | "WorktreeRemove" => &["cwd", "directory", "file_path", "worktree_path"],
        "PreModelSwitch" | "PostModelSwitch" => &["from_model", "to_model", "model", "source"],
        "UserPromptExpansion" => &["command", "command_name"],
        "MessageDisplay" => &["message_id", "index", "final"],
        "PostToolBatch" => &["tool_calls_count"],
        "ElicitationResult" => &["mcp_server_name", "action"],
        "Notification" | "StopFailure" => &["notification_type", "error_type"],
        _ => &[],
    };
    let parts: Vec<String> = fields.iter().filter_map(|key| input.get(*key).filter(|v| !v.is_null()).map(|v| {
        let value = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
        format!("{key}={}", value.chars().take(160).collect::<String>())
    })).collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

const PERMISSION_TITLE_CHARS: usize = 100;
const DESTRUCTIVE: &str = "⚠️ destructivo";

fn destructive(command: &str) -> bool {
    !alondra_core::allow_always::risks(command).is_empty()
}

/// What each risky part of a command does, in plain words, one line per kind of risk. The parts
/// come from the same reading that keeps «allow always» off them, so the two never disagree.
fn risks(command: &str) -> Vec<&'static str> {
    use alondra_core::allow_always::RiskKind;
    alondra_core::allow_always::risks(command).into_iter().map(|risk| match risk.kind {
        RiskKind::PushForce => "`push --force`: reescribe la rama remota; puede borrar commits que no tienes en local",
        RiskKind::WorktreeRemoveForce => "`worktree remove --force`: borra el worktree aunque tenga cambios sin commit",
        RiskKind::BranchForceDelete => "`branch -D`: borra la rama aunque no esté mergeada; sus commits sin push se pierden",
        RiskKind::ResetHard => "`reset --hard`: descarta todos los cambios sin commit",
        RiskKind::CleanForce => "`clean -f`: borra los ficheros que git no sigue",
        RiskKind::DiscardsChanges => "`checkout --` / `restore` / `stash drop`: descarta cambios sin commit",
        RiskKind::RewritesHistory => "`filter-branch` / `update-ref` / `reflog`: reescribe la historia de git",
        RiskKind::Deletes => "`rm` / `clean`: borra ficheros sin pasar por la papelera",
        RiskKind::Overwrites => "`cp` / `mv` / `chmod` / `sed -i`: sobrescribe o cambia ficheros",
        RiskKind::Force => "`--force`: se salta las comprobaciones de seguridad del comando",
        RiskKind::ForcedVariant => "`-D` / `--hard`: variante forzada del comando, sin comprobaciones",
    }).collect()
}

pub(crate) fn permission_title(tool: Option<&str>, input: &Value, summary: Option<&str>) -> String {
    let tool = tool.unwrap_or("?");
    let said = str_field(input, "description").map(|d| d.trim().to_string()).filter(|d| !d.is_empty()).or_else(|| summary.map(|s| s.split_whitespace().collect::<Vec<_>>().join(" ")));
    let title = match said {
        Some(s) if s.chars().count() > PERMISSION_TITLE_CHARS => format!("{tool}: {}…", s.chars().take(PERMISSION_TITLE_CHARS).collect::<String>().trim_end()),
        Some(s) => format!("{tool}: {s}"),
        None => tool.to_string(),
    };
    match str_field(input, "command") {
        Some(c) if destructive(&c) => format!("{DESTRUCTIVE} · {title}"),
        _ => title,
    }
}

pub(crate) fn permission_detail(input: &Value, summary: Option<&str>) -> Option<String> {
    if let Some(command) = str_field(input, "command") {
        let risks = risks(&command);
        if risks.is_empty() {
            return Some(format!("```\n{command}\n```"));
        }
        let lines = risks.iter().map(|r| format!("- {r}")).collect::<Vec<_>>().join("\n");
        return Some(format!("{lines}\n\n```\n{command}\n```"));
    }
    summary.map(str::to_string).or_else(|| (!input.is_null()).then(|| input.to_string()))
}

pub(crate) fn elicitation_card(p: &Value) -> (String, Option<String>, Vec<QuestionOption>) {
    let server = str_field(p, "mcp_server_name").unwrap_or_else(|| "An MCP server".into());
    let message = str_field(p, "message").unwrap_or_default();
    let title = message.lines().map(str::trim).find(|l| !l.is_empty()).map_or_else(|| format!("{server} requests your input"), |l| l.chars().take(120).collect());
    let fields = alondra_provider_claude::elicitation_fields(p.get("requested_schema").unwrap_or(&Value::Null));
    let mut detail = vec![format!("MCP server “{server}” requests your input."), message];
    if let Some(url) = str_field(p, "url") {
        detail.push(url);
    }
    let accept_hint = match fields.as_slice() {
        [] => None,
        [(_, title, _)] => Some(format!("Write the {title}")),
        many => Some(format!("Write one per line: {}", many.iter().map(|(name, _, _)| format!("{name}: …")).collect::<Vec<_>>().join(", "))),
    };
    (
        title,
        Some(detail.into_iter().filter(|d| !d.is_empty()).collect::<Vec<_>>().join("\n\n")),
        vec![
            QuestionOption { label: answer_option::ACCEPT.into(), description: accept_hint },
            QuestionOption { label: answer_option::DECLINE.into(), description: None },
        ],
    )
}

pub(crate) fn elicitation_content(schema: &Value, said: &str) -> Value {
    Value::Object(alondra_provider_claude::elicitation_content(schema, said))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_elicitation_becomes_an_accept_or_decline_card() {
        let context7 = json!({"session_id": "s", "hook_event_name": "Elicitation", "mcp_server_name": "context7", "mode": "form",
            "message": "You're using Context7 anonymously. Get a free API key for higher rate limits.",
            "requested_schema": {"type": "object", "properties": {}}});
        let login = json!({"session_id": "s", "hook_event_name": "Elicitation", "mcp_server_name": "db", "mode": "form", "message": "Log in",
            "requested_schema": {"type": "object", "properties": {"username": {"type": "string", "title": "Username"}, "port": {"type": "integer"}, "remember": {"type": "boolean"}}}});
        let token = json!({"session_id": "s", "hook_event_name": "Elicitation", "mcp_server_name": "gh", "mode": "form", "message": "Paste the token",
            "requested_schema": {"type": "object", "properties": {"token": {"type": "string", "title": "Token"}}}});
        let browser = json!({"session_id": "s", "hook_event_name": "Elicitation", "mcp_server_name": "auth", "mode": "url", "message": "Please authenticate", "url": "https://auth.example.com/login"});
        let cases = [
            ("context7's anonymous notice", &context7, "You're using Context7 anonymously. Get a free API key for higher rate limits.", None),
            ("a one-field form", &token, "Paste the token", Some("Write the Token")),
            ("a form with several fields", &login, "Log in", Some("Write one per line: port: …, remember: …, username: …")),
            ("a browser link", &browser, "Please authenticate", None),
        ];
        for (what, payload, title, accept_hint) in cases {
            let (said, detail, options) = elicitation_card(payload);
            assert_eq!(said, title, "{what}");
            assert_eq!(options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), [answer_option::ACCEPT, answer_option::DECLINE], "{what}");
            assert_eq!(options[0].description.as_deref(), accept_hint, "{what}");
            assert!(detail.is_some_and(|d| d.contains(str_field(payload, "mcp_server_name").unwrap().as_str())), "{what}");
        }
        assert!(elicitation_card(&browser).1.is_some_and(|d| d.contains("https://auth.example.com/login")));
        let schema = &login["requested_schema"];
        assert_eq!(elicitation_content(schema, "Username: ana\nport: 5432\nremember: yes"), json!({"username": "ana", "port": 5432, "remember": true}));
        assert_eq!(elicitation_content(schema, r#"{"username": "ana"}"#), json!({"username": "ana"}));
        assert_eq!(elicitation_content(&token["requested_schema"], "ghp_123"), json!({"token": "ghp_123"}));
    }

    #[test]
    fn a_permission_card_is_titled_by_what_the_node_says_it_does_and_shows_the_whole_command() {
        let command = "git worktree remove --force /Users/ana/Projects/lumo/.claude/worktrees/permiso-camara; git branch -D permiso-camara";
        let explained = "- `worktree remove --force`: borra el worktree aunque tenga cambios sin commit\n\
                         - `branch -D`: borra la rama aunque no esté mergeada; sus commits sin push se pierden";
        struct Case {
            what: &'static str,
            tool: &'static str,
            input: Value,
            title: String,
            detail: Option<String>,
        }
        let long = "x".repeat(150);
        let cases = [
            Case { what: "the real worktree cleanup, with its description", tool: "Bash",
                input: json!({"command": command, "description": "Remove the camera worktree and its branch"}),
                title: "⚠️ destructivo · Bash: Remove the camera worktree and its branch".into(), detail: Some(format!("{explained}\n\n```\n{command}\n```")) },
            Case { what: "the same command without a description", tool: "Bash", input: json!({"command": command}),
                title: format!("⚠️ destructivo · Bash: {}…", command.chars().take(PERMISSION_TITLE_CHARS).collect::<String>().trim_end()), detail: Some(format!("{explained}\n\n```\n{command}\n```")) },
            Case { what: "a harmless command", tool: "Bash", input: json!({"command": "ls -la", "description": "List files"}),
                title: "Bash: List files".into(), detail: Some("```\nls -la\n```".into()) },
            Case { what: "a file edit", tool: "Edit", input: json!({"file_path": "/tmp/a.rs", "old_string": "a", "new_string": "b"}),
                title: "Edit: /tmp/a.rs".into(), detail: Some("/tmp/a.rs".into()) },
            Case { what: "a long path is cut", tool: "Write", input: json!({"file_path": long}),
                title: format!("Write: {}…", "x".repeat(PERMISSION_TITLE_CHARS)), detail: Some(long.clone()) },
        ];
        for c in cases {
            let summary = tool_summary(Some(c.tool), Some(&c.input));
            assert_eq!(permission_title(Some(c.tool), &c.input, summary.as_deref()), c.title, "{}", c.what);
            assert_eq!(permission_detail(&c.input, summary.as_deref()), c.detail, "{}", c.what);
        }
    }

    #[test]
    fn destructive_commands_are_told_apart() {
        let cases = [
            ("git worktree remove --force x", true),
            ("git branch -D feature", true),
            ("rm -rf build", true),
            ("cd x && rm file", true),
            ("git reset --hard origin/main", true),
            ("git push --force-with-lease", true),
            ("git push -f origin main", true),
            ("git clean -fdx", true),
            ("git push origin main", false),
            ("git branch -d merged", false),
            ("cargo test --release", false),
            ("git clean -n", false),
            ("ls -f", false),
        ];
        for (command, expected) in cases {
            assert_eq!(destructive(command), expected, "{command}");
        }
    }

    #[test]
    fn each_risky_flag_is_explained_in_plain_words() {
        let cases: [(&str, &[&str]); 9] = [
            ("git push --force-with-lease origin x", &["`push --force`"]),
            ("git push -f", &["`push --force`"]),
            ("git worktree remove --force wt; git branch -D wt", &["`worktree remove --force`", "`branch -D`"]),
            ("git reset --hard origin/main", &["`reset --hard`"]),
            ("git clean -fdx", &["`clean -f`"]),
            ("cd build && rm -rf out", &["`rm` / `clean`"]),
            ("brew upgrade --force node", &["`--force`"]),
            ("git rm old.txt", &[]),
            ("git branch -d merged && git push origin main", &[]),
        ];
        for (command, expected) in cases {
            let said: Vec<&str> = risks(command).iter().map(|r| r.split(':').next().unwrap()).collect();
            assert_eq!(said, expected, "{command}");
        }
        assert_eq!(risks("rm a; rm b").len(), 1, "one line per kind of risk");
    }
}
