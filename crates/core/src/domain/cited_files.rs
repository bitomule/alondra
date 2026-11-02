//! A plan or a decision is read on its card alone: the user cannot open a Markdown file from the
//! app. The files its author names explicitly (the `files` field of a card, or a Markdown link in
//! a native plan) are included at the end of the card under a heading with their path. A path
//! merely mentioned in the text is never read and never refuses anything.
use super::checklist::task_text;

pub const CITED_FILE_MAX_BYTES: usize = 32 * 1024;
pub const CITED_FILES_MAX_BYTES: usize = 128 * 1024;

/// A Markdown file inside the author's worktree: relative, without `..`, ending in `.md`.
pub fn worktree_markdown_path(path: &str) -> bool {
    let file = std::path::Path::new(path);
    !path.starts_with('/') && !path.starts_with('~') && path.chars().all(path_char)
        && file.components().all(|part| matches!(part, std::path::Component::Normal(_) | std::path::Component::CurDir))
        && file.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        && file.file_stem().is_some_and(|stem| !stem.is_empty())
}

/// The worktree Markdown files `plan` links to with CommonMark inline links (`[text](path.md)`),
/// once each and in order: outside code, and before the first included file.
pub fn linked_markdown_files(plan: &str) -> Vec<String> {
    let mut fence: Option<&str> = None;
    let mut linked: Vec<String> = Vec::new();
    for line in plan.lines() {
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
        for path in link_destinations(line) {
            if worktree_markdown_path(&path) && !linked.contains(&path) {
                linked.push(path);
            }
        }
    }
    linked
}

/// The destinations of a line's inline links, outside code spans, without their fragment or title;
/// images are not links.
fn link_destinations(line: &str) -> Vec<String> {
    let mut destinations = Vec::new();
    let mut code = false;
    let mut label: Option<usize> = None;
    let bytes = line.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'`' => code = !code,
            b'[' if !code => label = Some(at),
            b']' if !code && label.is_some() && bytes.get(at + 1) == Some(&b'(') => {
                let opened = label.take().expect("checked");
                let rest = &line[at + 2..];
                let Some(close) = rest.find(')') else { break };
                let inside = rest[..close].trim();
                let target = inside.strip_prefix('<').and_then(|t| t.split_once('>')).map_or_else(
                    || inside.split_whitespace().next().unwrap_or_default(),
                    |(target, _)| target,
                );
                let target = target.split_once('#').map_or(target, |(file, _)| file);
                if opened == 0 || bytes[opened - 1] != b'!' {
                    destinations.push(target.strip_prefix("./").unwrap_or(target).to_owned());
                }
                at += 2 + close;
            }
            _ => {}
        }
        at += 1;
    }
    destinations
}

/// `plan` with the worktree Markdown files it links to included at its end, each read by `read`
/// from the plan's worktree. A link to a file that is not there includes nothing; a linked file
/// too long for a card is the reason the plan cannot be sent.
pub fn include_linked_files(plan: &str, read: impl FnMut(&str) -> Option<String>) -> Result<String, String> {
    include(plan, linked_markdown_files(plan), read, false)
}

/// `text` with each of `files`, the files the author listed, included at its end; the reason it
/// cannot be sent when one is not a worktree Markdown file, cannot be read or is too long.
pub fn include_files(text: &str, files: &[String], read: impl FnMut(&str) -> Option<String>) -> Result<String, String> {
    if let Some(path) = files.iter().find(|path| !worktree_markdown_path(path)) {
        return Err(not_a_worktree_file(path));
    }
    include(text, files.to_vec(), read, true)
}

/// Why a card that lists `files` cannot reach the user: a listed file is not a worktree Markdown
/// file, or `shown` does not include it; `None` when every listed file is there.
pub fn listed_files_problem(files: &[String], shown: &[&str]) -> Option<String> {
    if let Some(path) = files.iter().find(|path| !worktree_markdown_path(path)) {
        return Some(not_a_worktree_file(path));
    }
    let included: Vec<String> = shown.iter().flat_map(|text| included_files(text)).map(|(path, _)| path).collect();
    files.iter().find(|path| !included.contains(path))
        .map(|path| format!("the card lists {path} in files but does not include it under its heading `## {path}`: send the card through the Alondra tool, which reads the file from your worktree"))
}

fn not_a_worktree_file(path: &str) -> String {
    format!("{path} is not a Markdown file of your worktree: list paths relative to your worktree that end in .md, without .. (like docs/plan.md)")
}

fn include(text: &str, paths: Vec<String>, mut read: impl FnMut(&str) -> Option<String>, required: bool) -> Result<String, String> {
    let already: Vec<String> = included_files(text).into_iter().map(|(path, _)| path).collect();
    let mut included = text.trim_end().to_owned();
    let mut total = 0;
    for path in paths.into_iter().filter(|path| !already.contains(path)) {
        let Some(content) = read(&path) else {
            if required {
                return Err(format!("{path} cannot be read in your worktree: check the path, relative to your worktree, or remove it from files"));
            }
            continue;
        };
        if content.len() > CITED_FILE_MAX_BYTES {
            return Err(format!("{path} is {} KB, too long to show on a card (at most {} KB): quote the part that matters in the card instead",
                content.len() / 1024, CITED_FILE_MAX_BYTES / 1024));
        }
        total += content.len();
        if total > CITED_FILES_MAX_BYTES {
            return Err(format!("the files your card includes add up to more than {} KB: quote the parts that matter in the card instead", CITED_FILES_MAX_BYTES / 1024));
        }
        included.push_str(&format!("\n\n---\n\n## {path}\n\n{}", nested(&content)));
    }
    Ok(if included.len() == text.trim_end().len() { text.to_owned() } else { included.trim_start_matches('\n').to_owned() })
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

/// The files included at the end of `text`, in order: each path with its Markdown as the file
/// had it, its headings back at their own level.
pub fn included_files(text: &str) -> Vec<(String, String)> {
    let mut files: Vec<(String, Vec<&str>)> = Vec::new();
    let mut fence: Option<&str> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if fence.is_none()
            && let Some(path) = included_heading(line)
        {
            if let Some((_, lines)) = files.last_mut() {
                drop_separator(lines);
            }
            files.push((path.to_owned(), Vec::new()));
            continue;
        }
        let Some((_, lines)) = files.last_mut() else { continue };
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
        } else if let Some(marker) = ["```", "~~~"].into_iter().find(|marker| trimmed.starts_with(marker)) {
            fence = Some(marker);
        }
        lines.push(line);
    }
    files.into_iter().map(|(path, lines)| (path, unnested(&lines.join("\n")))).collect()
}

fn drop_separator(lines: &mut Vec<&str>) {
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    if lines.last().is_some_and(|line| line.trim() == "---") {
        lines.pop();
    }
}

fn unnested(content: &str) -> String {
    let mut fence: Option<&str> = None;
    let mut lines = Vec::new();
    for line in content.trim().lines() {
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            lines.push(line);
            continue;
        }
        if let Some(marker) = ["```", "~~~"].into_iter().find(|marker| trimmed.starts_with(marker)) {
            fence = Some(marker);
            lines.push(line);
            continue;
        }
        let level = line.bytes().take_while(|b| *b == b'#').count();
        lines.push(if (3..=6).contains(&level) && line[level..].starts_with(' ') { &line[2..] } else { line });
    }
    lines.join("\n")
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

/// The path of a line that opens an included file: `## <path>.md`, as Alondra writes it.
fn included_heading(line: &str) -> Option<&str> {
    let path = line.strip_prefix("## ")?.trim();
    let file = std::path::Path::new(path);
    (path.chars().all(path_char) && file.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        && file.file_stem().is_some_and(|stem| stem.to_str().is_some_and(|stem| stem.chars().any(|c| c.is_ascii_alphanumeric()))))
    .then_some(path)
}

fn path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '~' | '+' | '@')
}
