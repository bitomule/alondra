//! The status line of the sessions Alondra launches: it runs the status line the user set up, so
//! the session shows the same line it would without Alondra.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Result;
use serde_json::Value;

pub fn run() -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let parsed: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let Some(command) = user_command(&parsed, &claude_home()) else { return Ok(()) };
    let Ok(mut child) = Command::new("/bin/sh").arg("-c").arg(&command).stdin(Stdio::piped()).stdout(Stdio::inherit()).spawn() else {
        return Ok(());
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let _ = child.wait();
    Ok(())
}

fn claude_home() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".claude"))
}

/// The status line the session would show without Alondra's: the project's own, else the user's.
fn user_command(input: &Value, home: &Path) -> Option<String> {
    let project = input.pointer("/workspace/project_dir").or_else(|| input.get("cwd")).and_then(Value::as_str).map(|d| Path::new(d).join(".claude"));
    let files = project.iter().flat_map(|p| [p.join("settings.local.json"), p.join("settings.json")]).chain([home.join("settings.json")]);
    files
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .filter_map(|text| serde_json::from_str::<Value>(&text).ok())
        .find_map(|settings| {
            let line = settings.get("statusLine")?;
            let command = line.get("command")?.as_str()?;
            let own = command.contains("alondra' statusline") || command.ends_with("alondra statusline");
            (line.get("type").and_then(Value::as_str) == Some("command") && !own).then(|| command.to_string())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_status_line_shown_is_the_projects_else_the_users_and_never_alondras_own() {
        let root = std::env::temp_dir().join(format!("alondra-statusline-{}", std::process::id()));
        let (home, project) = (root.join("home"), root.join("project"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        let input = json!({"workspace": {"project_dir": project.display().to_string()}});
        assert_eq!(user_command(&input, &home), None, "no status line set up");

        std::fs::write(home.join("settings.json"), r#"{"statusLine": {"type": "command", "command": "~/line.sh"}}"#).unwrap();
        assert_eq!(user_command(&input, &home).as_deref(), Some("~/line.sh"));
        std::fs::write(project.join(".claude/settings.json"), r#"{"statusLine": {"type": "command", "command": "./project.sh"}}"#).unwrap();
        assert_eq!(user_command(&input, &home).as_deref(), Some("./project.sh"));
        std::fs::write(project.join(".claude/settings.json"), r#"{"statusLine": {"type": "command", "command": "'/r/plugin/bin/alondra' statusline"}}"#).unwrap();
        assert_eq!(user_command(&input, &home).as_deref(), Some("~/line.sh"), "Alondra's own line would call itself forever");

        std::fs::remove_dir_all(root).unwrap();
    }
}
