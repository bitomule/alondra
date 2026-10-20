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
            if touches_key(command) {
                return Some("Nodes cannot touch the user's signing key.".into());
            }
            let commands = simple_commands(command);
            if commands.iter().flatten().any(|word| reads_app_data(word)) {
                return Some(APP_DATA_REFUSAL.into());
            }
            commands.iter().any(|words| walks_disk_or_home(words)).then(|| WALK_REFUSAL.into())
        }
        tool => {
            let paths = ["file_path", "path", "pattern", "notebook_path"].map(field);
            if paths.iter().any(|path| touches_key(path)) {
                return Some("Nodes cannot touch the user's signing key.".into());
            }
            let pattern_root = if tool == "Glob" { literal_directory(field("pattern")) } else { String::new() };
            if paths.iter().copied().chain([pattern_root.as_str()]).any(reads_app_data) {
                return Some(APP_DATA_REFUSAL.into());
            }
            let searches = matches!(tool, "Grep" | "Glob");
            (searches && [field("path"), pattern_root.as_str()].into_iter().any(is_walk_root)).then(|| WALK_REFUSAL.into())
        }
    }
}

const WALK_REFUSAL: &str = "Do not search or measure the whole disk or home folder: walking it makes macOS ask the user to let Alondra read other apps' data. Search your worktree or the exact folder (node_modules, Bazel's output base), find a file by name with `mdfind`, and check free space with `df`.";
const APP_DATA_REFUSAL: &str = "Nodes do not read other apps' data in ~/Library/Containers or ~/Library/Group Containers: macOS asks the user for permission every time.";
const APP_DATA: [&str; 2] = ["Library/Containers", "Library/Group Containers"];

/// The words of each simple command, without quotes or escapes: `~/Library/'Group Containers'`
/// is one word.
fn simple_commands(command: &str) -> Vec<Vec<String>> {
    let (mut commands, mut words, mut word) = (vec![], vec![], String::new());
    let mut quote = None;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (Some('"'), '\\') | (None, '\\') => word.extend(chars.next()),
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => quote = Some(c),
            (None, c) if c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '`' | '<' | '>') => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                let ends_command = c == '\n' || !(c.is_whitespace() || matches!(c, '<' | '>'));
                if ends_command && !words.is_empty() {
                    commands.push(std::mem::take(&mut words));
                }
            }
            (None, c) => word.push(c),
        }
    }
    words.extend((!word.is_empty()).then_some(word));
    commands.extend((!words.is_empty()).then_some(words));
    commands
}

/// A recursive search or size of `/`, the home folder, everything in it or its whole Library.
fn walks_disk_or_home(words: &[String]) -> bool {
    words.iter().enumerate().any(|(at, word)| {
        let arguments = &words[at + 1..];
        let short_flag = |letters: &[char]| arguments.iter().any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains(letters));
        let recursive = match word.rsplit('/').next().unwrap_or(word) {
            "find" | "bfs" | "fd" | "du" | "rg" | "tree" => true,
            "grep" => short_flag(&['r', 'R']) || arguments.iter().any(|a| a.starts_with("--recursive") || a.starts_with("--dereference-recursive")),
            "ls" => short_flag(&['R']),
            _ => false,
        };
        recursive && arguments.iter().any(|argument| is_walk_root(argument))
    })
}

fn is_walk_root(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let path = path.trim_end_matches('/');
    if ["", "/Users", "/System/Volumes/Data", "/*"].contains(&path) {
        return true;
    }
    home_relative(path).is_some_and(|rest| rest.is_empty() || rest == "Library" || (!rest.contains('/') && rest.contains(['*', '?', '['])))
}

fn reads_app_data(path: &str) -> bool {
    home_relative(path.trim_end_matches('/'))
        .is_some_and(|rest| APP_DATA.iter().any(|dir| rest.strip_prefix(dir).is_some_and(|tail| tail.is_empty() || tail.starts_with('/'))))
}

/// The path after the home folder, empty for the home folder itself.
fn home_relative(path: &str) -> Option<&str> {
    for home in ["~", "$HOME", "${HOME}"] {
        if let Some(rest) = path.strip_prefix(home) {
            return if rest.is_empty() { Some("") } else { rest.strip_prefix('/') };
        }
    }
    let in_users = path.strip_prefix("/Users/")?;
    let (user, rest) = in_users.split_once('/').unwrap_or((in_users, ""));
    (!user.is_empty() && user != "Shared").then_some(rest)
}

/// The folder a glob pattern starts from: `/Users/x/**/a.ts` starts from `/Users/x`.
fn literal_directory(pattern: &str) -> String {
    let Some(wildcard) = pattern.find(['*', '?', '[', '{']) else { return String::new() };
    let literal = &pattern[..wildcard];
    match literal.rfind('/') {
        Some(0) => "/".into(),
        Some(end) => literal[..end].into(),
        None => String::new(),
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
    fn nodes_never_walk_the_disk_or_the_home_folder() {
        for command in [
            r#"cat agent-host/package.json && ls agent-host/node_modules 2>/dev/null | head; find / -path /System -prune -o -type d -name "claude-agent-sdk" -print 2>/dev/null | grep -v "^/System" | head"#,
            r#"find / -path "*claude-agent-sdk*/sdk.d.ts" -maxdepth 8 2>/dev/null | head -5"#,
            "du -sh ~/* ~/.[a-z]* 2>/dev/null | sort -h | tail -12",
            "du -sh ~/Projects ~/Library ~/Downloads 2>/dev/null",
            "timeout 60 rg -l sdk.d.ts $HOME/",
            "grep -rn claude-agent-sdk /Users/me",
            "ls -laR ~ | wc -l",
        ] {
            assert_eq!(bash(command).as_deref(), Some(WALK_REFUSAL), "{command}");
        }
        let search = |tool: &str, input: Value| forbidden_for_nodes(Some(tool), Some(&input));
        assert_eq!(search("Grep", json!({"pattern": "sdk", "path": "/Users/me/"})).as_deref(), Some(WALK_REFUSAL));
        assert_eq!(search("Glob", json!({"pattern": "/**/sdk.d.ts"})).as_deref(), Some(WALK_REFUSAL));
        assert_eq!(search("Glob", json!({"pattern": "~/Library/**/*.plist"})).as_deref(), Some(WALK_REFUSAL));
    }

    #[test]
    fn nodes_never_read_other_apps_data() {
        for command in [
            r"du -sh ~/Library/Developer ~/Library/Group\ Containers 2>/dev/null",
            "du -sh ~/Library/Containers/com.docker.docker",
            "ls \"$HOME/Library/Group Containers/group.com.apple.notes\"",
            "cat /Users/me/Library/'Group Containers'/x/prefs.plist",
        ] {
            assert_eq!(bash(command).as_deref(), Some(APP_DATA_REFUSAL), "{command}");
        }
        let read = forbidden_for_nodes(Some("Read"), Some(&json!({"file_path": "/Users/me/Library/Containers/com.apple.mail/Data/x"})));
        assert_eq!(read.as_deref(), Some(APP_DATA_REFUSAL));
    }

    #[test]
    fn searches_of_a_known_folder_pass() {
        for command in [
            "find . -name '*.rs' -path '*/src/*'",
            "find ~/Projects/alondra -maxdepth 3 -name sdk.d.ts",
            "du -sh ~/Projects/alondra/.claude/worktrees/* ~/Library/Developer/CoreSimulator ~/Library/Caches",
            "df -h / && mdfind -name sdk.d.ts",
            "rg -n Containers crates/core/src",
            "grep -n 'find /' docs/notes.md",
            "ls -la ~ /",
            "cd / && git -C ~/Projects/alondra status",
        ] {
            assert_eq!(bash(command), None, "{command}");
        }
        assert!(forbidden_for_nodes(Some("Grep"), Some(&json!({"pattern": "x", "path": "/Users/me/Projects/alondra"}))).is_none());
        assert!(forbidden_for_nodes(Some("Glob"), Some(&json!({"pattern": "**/*.rs"}))).is_none());
    }

    #[test]
    fn ordinary_commands_pass() {
        assert!(bash("alondra status").is_none());
        assert!(bash("alondra doctor --json").is_none());
        assert!(bash("git commit -m 'answer the question'").is_none());
        assert!(forbidden_for_nodes(Some("Read"), Some(&json!({"file_path": "/tmp/x.md"}))).is_none());
    }
}
