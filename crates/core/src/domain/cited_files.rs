//! A plan or a decision is read on its card alone: the user cannot open a Markdown file the
//! author cites from their worktree. Each cited file is included at the end of the card under a
//! heading with its path, or the card is refused.
use super::checklist::task_text;

pub const CITED_FILE_MAX_BYTES: usize = 32 * 1024;
pub const CITED_FILES_MAX_BYTES: usize = 128 * 1024;

/// The local Markdown files `text` cites, once each and in order: outside code blocks and
/// checklist lines (files the work will deliver), and before the first included file, whose own
/// citations are not followed.
pub fn cited_markdown_files(text: &str) -> Vec<String> {
    let mut fence: Option<&str> = None;
    let mut cited: Vec<String> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if let Some(marker) = ["```", "~~~"].into_iter().find(|marker| trimmed.starts_with(marker)) {
            fence = Some(marker);
            continue;
        }
        if included_heading(line).is_some() {
            break;
        }
        if task_text(trimmed).is_some() {
            continue;
        }
        for path in markdown_paths(line) {
            if !cited.contains(&path) {
                cited.push(path);
            }
        }
    }
    cited
}

/// What the author wrote, without the files included at its end.
pub fn without_included_files(text: &str) -> &str {
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        if included_heading(line.trim_end_matches(['\r', '\n'])).is_some() {
            let before = text[..at].trim_end();
            return before.strip_suffix("---").map_or(before, str::trim_end);
        }
        at += line.len();
    }
    text
}

/// The cited files whose content `text` does not include yet.
pub fn files_not_included(text: &str) -> Vec<String> {
    let included: Vec<&str> = text.lines().filter_map(included_heading).collect();
    cited_markdown_files(text).into_iter().filter(|path| !included.contains(&path.as_str())).collect()
}

/// Why a card that cites files it does not include cannot reach the user, as a refusal code and
/// how to fix it; `None` when it stands alone.
pub fn cited_files_problem(text: &str) -> Option<(&'static str, String)> {
    let missing = files_not_included(text);
    (!missing.is_empty()).then(|| ("cited_file_not_included", not_openable(&missing.join(", "))))
}

fn not_openable(paths: &str) -> String {
    format!("the user cannot open {paths} from the app, and your card must be read on its own: write the content you mean into the card itself, or send the file as an Alondra document; if it is your finished work, deliver it with present_result and the whole report in content.body")
}

/// `text` with every file it cites included at its end, each read by `read` from where the
/// author works (`None` when it cannot be read); the reason it cannot be sent otherwise.
pub fn include_cited_files(text: &str, mut read: impl FnMut(&str) -> Option<String>) -> Result<String, String> {
    let missing = files_not_included(text);
    if missing.is_empty() {
        return Ok(text.to_owned());
    }
    let mut included = text.trim_end().to_owned();
    let mut total = 0;
    for path in missing {
        let content = read(&path).ok_or_else(|| not_openable(&path))?;
        if content.len() > CITED_FILE_MAX_BYTES {
            return Err(format!("{path} is {} KB, too long to show on a card (at most {} KB): quote the part that matters in the card and drop the path",
                content.len() / 1024, CITED_FILE_MAX_BYTES / 1024));
        }
        total += content.len();
        if total > CITED_FILES_MAX_BYTES {
            return Err(format!("the files your card cites add up to more than {} KB: quote the parts that matter in the card and drop their paths", CITED_FILES_MAX_BYTES / 1024));
        }
        included.push_str(&format!("\n\n---\n\n## {path}\n\n{}", nested(&content)));
    }
    Ok(included)
}

/// A file's Markdown placed under its heading: its headings two levels down, its checkboxes shown
/// as ☐/☑ so they are never points of the plan, and an open code block closed.
fn nested(content: &str) -> String {
    let mut fence: Option<&str> = None;
    let mut lines = Vec::new();
    for line in content.trim_end().lines() {
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            lines.push(line.to_owned());
            continue;
        }
        if let Some(marker) = ["```", "~~~"].into_iter().find(|marker| trimmed.starts_with(marker)) {
            fence = Some(marker);
            lines.push(line.to_owned());
            continue;
        }
        let level = trimmed.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&level) && trimmed[level..].starts_with(' ') {
            lines.push(format!("{} {}", "#".repeat((level + 2).min(6)), trimmed[level..].trim_start()));
            continue;
        }
        lines.push(match task_text(trimmed) {
            Some(_) => unchecked(line),
            None => line.to_owned(),
        });
    }
    if let Some(open) = fence {
        lines.push(open.to_owned());
    }
    lines.join("\n")
}

fn unchecked(line: &str) -> String {
    ["[ ] ", "[x] ", "[X] "].into_iter().find_map(|box_| {
        let at = line.find(box_)?;
        let mark = if box_ == "[ ] " { "☐ " } else { "☑ " };
        Some(format!("{}{mark}{}", &line[..at], &line[at + box_.len()..]))
    }).unwrap_or_else(|| line.to_owned())
}

/// The path of a line that opens an included file: `## <path>.md`.
fn included_heading(line: &str) -> Option<&str> {
    let path = line.strip_prefix("## ")?.trim();
    (markdown_paths(path).first().map(String::as_str) == Some(path)).then_some(path)
}

fn path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '~' | '+' | '@')
}

/// Paths ending in `.md` in a line, never part of a URL.
fn markdown_paths(line: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let lower = line.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find(".md") {
        let end = from + found + 3;
        from = end;
        let rest = &line[end..];
        let mut after = rest.chars();
        let ends_word = match after.next() {
            None => true,
            Some(c) if c.is_ascii_alphanumeric() || c == '_' => false,
            Some('.' | '-' | '/') => !after.next().is_some_and(|c| c.is_ascii_alphanumeric()),
            Some(_) => true,
        };
        if !ends_word {
            continue;
        }
        let start = line[..end].char_indices().rev().take_while(|(_, c)| path_char(*c)).last().map_or(end, |(at, _)| at);
        let path = &line[start..end];
        let stem = &path[..path.len() - 3];
        let host = path.split_once('/').is_some_and(|(first, _)| first.contains('.') && !first.starts_with('.'));
        if stem.is_empty() || stem.ends_with('/') || stem.ends_with('.') || path.starts_with("//") || host || line[..start].ends_with(':') || !stem.chars().any(|c| c.is_ascii_alphanumeric()) {
            continue;
        }
        paths.push(path.to_owned());
    }
    paths
}
