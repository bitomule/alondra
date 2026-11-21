//! The conversation of a session that ends, condensed from Claude Code's native transcript into
//! Markdown a new session searches instead of reading: an index of its turns, then each turn with
//! what the user and Claude wrote in full, one line per tool call and the first lines of each tool
//! result. No model is asked. The transcript's format is Claude Code's own and undocumented: a line
//! that is not JSON, or an entry it does not know, is left out.
use serde_json::Value;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The heading each turn starts with, which the new session searches for.
pub const TURN: &str = "## Turn ";
/// How many lines of a tool result are kept, and how many characters of each line.
const RESULT_LINES: usize = 4;
const LINE_CHARACTERS: usize = 200;
/// How many files a line of the index names before it only counts the rest.
const INDEX_FILES: usize = 5;

#[derive(Default)]
struct Turn {
    at: Option<String>,
    user: String,
    body: String,
    files: Vec<String>,
}

/// The Markdown of the conversation in `jsonl`, the transcript at `original`, whose subagents'
/// transcripts are `subagents`.
pub fn condensed(jsonl: &str, original: &Path, subagents: &[PathBuf]) -> String {
    let mut turns: Vec<Turn> = Vec::new();
    for (index, line) in jsonl.lines().enumerate() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        if entry["isSidechain"].as_bool() == Some(true) || entry["isMeta"].as_bool() == Some(true) {
            continue;
        }
        let content = &entry["message"]["content"];
        match entry["type"].as_str() {
            Some("user") => {
                let text = said(content);
                if !text.trim().is_empty() && !has_block(content, "tool_result") {
                    turns.push(Turn { at: time(&entry), user: text, ..Turn::default() });
                } else if let Some(turn) = turns.last_mut() {
                    for block in blocks(content).filter(|block| block["type"] == "tool_result") {
                        result(&mut turn.body, &block["content"], block["is_error"].as_bool() == Some(true), index + 1);
                    }
                }
            }
            Some("assistant") => {
                let Some(turn) = turns.last_mut() else { continue };
                for block in blocks(content) {
                    match block["type"].as_str() {
                        Some("text") => if let Some(text) = block["text"].as_str().filter(|text| !text.trim().is_empty()) {
                            let _ = write!(turn.body, "\n**Claude**\n\n{}\n", escaped(text.trim()));
                        },
                        Some("tool_use") => tool_call(turn, block),
                        _ => {}
                    }
                }
                if let Value::String(text) = content && !text.trim().is_empty() {
                    let _ = write!(turn.body, "\n**Claude**\n\n{}\n", escaped(text.trim()));
                }
            }
            _ => {}
        }
    }
    let mut out = String::from("# The conversation before this session\n\n");
    let _ = writeln!(out, "Condensed from the session's transcript. Each turn starts with a line `{TURN}N`; a tool result is cut to its first lines, and `full: jsonl line L` names the line of the transcript that holds all of it.\n");
    out.push_str("## Index\n\n");
    if turns.is_empty() {
        out.push_str("No turn was found in the transcript.\n");
    }
    for (number, turn) in turns.iter().enumerate() {
        let first = turn.user.lines().find(|line| !line.trim().is_empty()).unwrap_or_default();
        let _ = write!(out, "- Turn {} · {} · {}", number + 1, turn.at.as_deref().unwrap_or("unknown time"), clipped(first.trim()));
        if !turn.files.is_empty() {
            let named: Vec<&str> = turn.files.iter().take(INDEX_FILES).map(String::as_str).collect();
            let _ = write!(out, " · files: {}", named.join(", "));
            if turn.files.len() > INDEX_FILES {
                let _ = write!(out, " and {} more", turn.files.len() - INDEX_FILES);
            }
        }
        out.push('\n');
    }
    for (number, turn) in turns.iter().enumerate() {
        let _ = write!(out, "\n{TURN}{} · {}\n\n**User**\n\n{}\n{}", number + 1, turn.at.as_deref().unwrap_or("unknown time"), escaped(turn.user.trim()), turn.body);
    }
    out.push_str("\n## Sources\n\n");
    let _ = writeln!(out, "- Transcript: {}", original.display());
    if subagents.is_empty() {
        out.push_str("- Subagent transcripts: none\n");
    }
    for subagent in subagents {
        let _ = writeln!(out, "- Subagent transcript: {}", subagent.display());
    }
    out
}

fn blocks(content: &Value) -> impl Iterator<Item = &Value> {
    content.as_array().into_iter().flatten()
}

fn has_block(content: &Value, kind: &str) -> bool {
    blocks(content).any(|block| block["type"] == kind)
}

/// The text of a message: a plain string, or its text blocks one after the other.
fn said(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        _ => blocks(content).filter(|block| block["type"] == "text").filter_map(|block| block["text"].as_str()).collect::<Vec<_>>().join("\n\n"),
    }
}

/// The minute an entry was written, in UTC, as `2026-11-19 10:02`.
fn time(entry: &Value) -> Option<String> {
    let stamp = entry["timestamp"].as_str()?;
    let (day, clock) = stamp.split_once('T')?;
    (day.len() == 10 && clock.len() >= 5).then(|| format!("{day} {}", &clock[..5]))
}

/// One line for a tool call: its name and the path, command or other subject it acts on.
fn tool_call(turn: &mut Turn, block: &Value) {
    let input = &block["input"];
    let name = block["name"].as_str().unwrap_or("tool");
    let file = ["file_path", "notebook_path"].iter().find_map(|key| input[*key].as_str());
    if let Some(file) = file && !turn.files.iter().any(|known| known == file) {
        turn.files.push(file.to_owned());
    }
    let subject = file.or_else(|| ["command", "path", "pattern", "url", "query", "description", "prompt"].iter().find_map(|key| input[*key].as_str()));
    match subject {
        Some(subject) => { let _ = write!(turn.body, "\n- {name}: `{}`", clipped(subject.lines().next().unwrap_or_default().trim()).replace('`', "'")); }
        None => { let _ = write!(turn.body, "\n- {name}"); }
    }
}

/// The first lines of a tool result, and the line of the transcript that holds it whole.
fn result(body: &mut String, content: &Value, error: bool, line: usize) {
    let text = match content {
        Value::String(text) => text.clone(),
        _ => said(content),
    };
    let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    let label = if error { "error" } else { "result" };
    let _ = write!(body, "\n  - {label} ({} lines, full: jsonl line {line})", text.lines().count());
    for kept in lines.iter().take(RESULT_LINES) {
        let _ = write!(body, "\n    > {}", clipped(kept.trim_end()));
    }
    if lines.len() > RESULT_LINES {
        body.push_str("\n    > …");
    }
    body.push('\n');
}

fn clipped(text: &str) -> String {
    match text.char_indices().nth(LINE_CHARACTERS) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

/// `text` with no line that a search for the turns would take for one.
fn escaped(text: &str) -> String {
    text.lines().map(|line| if line.starts_with(TURN) { format!("\\{line}") } else { line.to_owned() }).collect::<Vec<_>>().join("\n")
}

#[cfg(all(test, not(feature = "real-hosts")))]
mod tests {
    use super::*;

    /// A short session as Claude Code writes it: a prompt, a reply that reads and edits files and
    /// runs a command, their results, a second prompt, and entries a condensation leaves out.
    const TRANSCRIPT: &str = concat!(
        r#"{"type":"summary","summary":"Release notes","leafUuid":"00000000-0000-4000-8000-000000000009"}"#, "\n",
        r#"{"type":"user","timestamp":"2026-11-19T10:02:11.000Z","message":{"role":"user","content":"Write the release notes\nfor version 2.4"}}"#, "\n",
        r#"{"type":"assistant","timestamp":"2026-11-19T10:02:15.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"private"},{"type":"text","text":"I'll read the changelog first."},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/Users/ana/demo/CHANGELOG.md"}}]}}"#, "\n",
        r#"{"type":"user","timestamp":"2026-11-19T10:02:16.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"line one\nline two\nline three\nline four\nline five\nline six"}]}}"#, "\n",
        "not json at all\n",
        r#"{"type":"assistant","timestamp":"2026-11-19T10:03:00.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"/Users/ana/demo/NOTES.md","old_string":"a","new_string":"b"}},{"type":"tool_use","id":"t3","name":"Bash","input":{"command":"make test\necho done"}}]}}"#, "\n",
        r#"{"type":"user","timestamp":"2026-11-19T10:03:05.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t3","is_error":true,"content":[{"type":"text","text":"1 test failed"}]}]}}"#, "\n",
        r#"{"type":"user","isSidechain":true,"timestamp":"2026-11-19T10:03:06.000Z","message":{"role":"user","content":"a subagent's own prompt"}}"#, "\n",
        r#"{"type":"user","isMeta":true,"timestamp":"2026-11-19T10:03:07.000Z","message":{"role":"user","content":"a caveat Claude Code adds"}}"#, "\n",
        r#"{"type":"file-history-snapshot","snapshot":{}}"#, "\n",
        r#"{"type":"assistant","timestamp":"2026-11-19T10:03:30.000Z","message":{"role":"assistant","content":[{"type":"text","text":"The notes are written.\n## Turn 7 is not a turn"}]}}"#, "\n",
        r#"{"type":"user","timestamp":"2026-11-19T14:40:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Now the changelog"}]}}"#, "\n",
    );

    fn condensation() -> String {
        condensed(TRANSCRIPT, Path::new("/Users/ana/.claude/projects/demo/s.jsonl"), &[PathBuf::from("/Users/ana/.claude/projects/demo/s/subagents/agent-1.jsonl")])
    }

    #[test]
    fn the_index_names_each_turn_its_time_first_line_and_files() {
        let text = condensation();
        assert!(text.contains("- Turn 1 · 2026-11-19 10:02 · Write the release notes · files: /Users/ana/demo/CHANGELOG.md, /Users/ana/demo/NOTES.md\n"), "{text}");
        assert!(text.contains("- Turn 2 · 2026-11-19 14:40 · Now the changelog\n"), "{text}");
        let headings: Vec<&str> = text.lines().filter(|line| line.starts_with(TURN)).collect();
        assert_eq!(headings, ["## Turn 1 · 2026-11-19 10:02", "## Turn 2 · 2026-11-19 14:40"], "only real turns start with the heading");
    }

    #[test]
    fn a_turn_keeps_every_word_one_line_per_tool_call_and_the_start_of_each_result() {
        let text = condensation();
        let first = &text[text.find("## Turn 1").unwrap()..text.find("## Turn 2").unwrap()];
        assert!(first.contains("Write the release notes\nfor version 2.4"), "{first}");
        assert!(first.contains("**Claude**\n\nI'll read the changelog first."), "{first}");
        assert!(first.contains("- Read: `/Users/ana/demo/CHANGELOG.md`"), "{first}");
        assert!(first.contains("  - result (6 lines, full: jsonl line 4)\n    > line one\n    > line two\n    > line three\n    > line four\n    > …"), "{first}");
        assert!(!first.contains("line five"), "{first}");
        assert!(first.contains("- Edit: `/Users/ana/demo/NOTES.md`\n- Bash: `make test`"), "{first}");
        assert!(first.contains("  - error (1 lines, full: jsonl line 7)\n    > 1 test failed"), "{first}");
        assert!(first.contains("The notes are written.\n\\## Turn 7 is not a turn"), "{first}");
        for left_out in ["private", "a subagent's own prompt", "a caveat Claude Code adds", "Release notes\"", "not json"] {
            assert!(!text.contains(left_out), "{left_out}: {text}");
        }
    }

    #[test]
    fn the_end_names_the_transcript_and_its_subagents() {
        let text = condensation();
        assert!(text.ends_with("## Sources\n\n- Transcript: /Users/ana/.claude/projects/demo/s.jsonl\n- Subagent transcript: /Users/ana/.claude/projects/demo/s/subagents/agent-1.jsonl\n"), "{text}");
        let empty = condensed("{}\nnonsense\n", Path::new("/t.jsonl"), &[]);
        assert!(empty.contains("No turn was found in the transcript.") && empty.ends_with("- Subagent transcripts: none\n"), "{empty}");
    }
}
