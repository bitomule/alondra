//! Reads `gh pr diff` output: files with their status, hunks, and each line with its old and new
//! numbers and the part of it that changed. The iPhone and Mac read the same text the same way
//! (`apple/Design/Sources/Diff/UnifiedDiff.swift`).

use std::ops::Range;

/// A file with more changed lines than this starts folded.
pub const FOLDED_CHANGE_THRESHOLD: usize = 400;
/// Longer lines are not compared for what changed inside them.
pub const LONGEST_COMPARED_LINE: usize = 400;

const GENERATED_NAMES: &[&str] = &[
    "Cargo.lock", "Package.resolved", "MODULE.bazel.lock", "package-lock.json", "yarn.lock", "pnpm-lock.yaml",
    "Podfile.lock", "Gemfile.lock", "go.sum", "poetry.lock", "uv.lock", "flake.lock",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Added,
    Deleted,
    Renamed,
    Modified,
}

impl Status {
    pub fn badge(self, binary: bool) -> char {
        match self {
            _ if binary => 'B',
            Status::Added => 'A',
            Status::Deleted => 'D',
            Status::Renamed => 'R',
            Status::Modified => 'M',
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Hunk,
    Context,
    Added,
    Removed,
    /// `\ No newline at end of file`.
    Note,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub op: Op,
    pub text: String,
    pub old: Option<u32>,
    pub new: Option<u32>,
    /// The bytes of `text` that changed against its paired line.
    pub changed: Option<Range<usize>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct File {
    pub path: String,
    pub old_path: Option<String>,
    pub status: Status,
    pub binary: bool,
    pub lines: Vec<Line>,
}

impl File {
    pub fn added(&self) -> usize {
        self.lines.iter().filter(|line| line.op == Op::Added).count()
    }

    pub fn removed(&self) -> usize {
        self.lines.iter().filter(|line| line.op == Op::Removed).count()
    }

    pub fn generated(&self) -> bool {
        let name = self.path.rsplit('/').next().unwrap_or(&self.path);
        GENERATED_NAMES.contains(&name)
            || name.ends_with(".xcstrings")
            || self.path.split('/').any(|part| part == "__Snapshots__")
    }

    pub fn large(&self) -> bool {
        self.added() + self.removed() > FOLDED_CHANGE_THRESHOLD
    }

    pub fn has_code(&self) -> bool {
        self.lines.iter().any(|line| line.op != Op::Note)
    }

    pub fn starts_folded(&self) -> bool {
        self.binary || !self.has_code() || self.generated() || self.large()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diff {
    pub files: Vec<File>,
    /// The daemon cut the diff: the rest is only on GitHub.
    pub truncated: bool,
}

impl Diff {
    pub fn read(text: &str, truncated: bool) -> Self {
        Diff { files: parse(text), truncated }
    }

    pub fn added(&self) -> usize {
        self.files.iter().map(File::added).sum()
    }

    pub fn removed(&self) -> usize {
        self.files.iter().map(File::removed).sum()
    }
}

pub fn parse(text: &str) -> Vec<File> {
    let mut files = vec![];
    let mut current: Option<File> = None;
    let (mut old, mut new, mut in_hunk) = (0u32, 0u32, false);
    let close = |current: &mut Option<File>, files: &mut Vec<File>| {
        if let Some(mut file) = current.take() {
            mark_changes(&mut file.lines);
            files.push(file);
        }
    };
    for raw in text.split('\n') {
        if let Some(header) = raw.strip_prefix("diff --git ") {
            close(&mut current, &mut files);
            in_hunk = false;
            current = Some(File {
                path: header.rsplit_once(" b/").map_or(header, |(_, path)| path).to_owned(),
                old_path: None,
                status: Status::Modified,
                binary: false,
                lines: vec![],
            });
            continue;
        }
        let Some(file) = current.as_mut() else { continue };
        if !in_hunk {
            if raw.starts_with("new file mode") {
                file.status = Status::Added;
                continue;
            }
            if raw.starts_with("deleted file mode") {
                file.status = Status::Deleted;
                continue;
            }
            if let Some(from) = raw.strip_prefix("rename from ") {
                file.old_path = Some(from.to_owned());
                file.status = Status::Renamed;
                continue;
            }
            if let Some(to) = raw.strip_prefix("rename to ") {
                file.path = to.to_owned();
                continue;
            }
            if raw.starts_with("Binary files ") || raw == "GIT binary patch" {
                file.binary = true;
                continue;
            }
            if raw.starts_with("--- ") {
                continue;
            }
            if let Some(target) = raw.strip_prefix("+++ ") {
                if target != "/dev/null" {
                    file.path = target.strip_prefix("b/").unwrap_or(target).to_owned();
                }
                continue;
            }
        }
        if raw.starts_with("@@") {
            (old, new) = hunk_starts(raw);
            in_hunk = true;
            file.lines.push(Line { op: Op::Hunk, text: raw.to_owned(), old: None, new: None, changed: None });
            continue;
        }
        if !in_hunk {
            continue;
        }
        let Some(first) = raw.chars().next() else { continue };
        let text = raw[first.len_utf8()..].to_owned();
        let line = match first {
            '+' => {
                new += 1;
                Line { op: Op::Added, text, old: None, new: Some(new - 1), changed: None }
            }
            '-' => {
                old += 1;
                Line { op: Op::Removed, text, old: Some(old - 1), new: None, changed: None }
            }
            ' ' => {
                (old, new) = (old + 1, new + 1);
                Line { op: Op::Context, text, old: Some(old - 1), new: Some(new - 1), changed: None }
            }
            '\\' => Line { op: Op::Note, text: text.trim().to_owned(), old: None, new: None, changed: None },
            _ => continue,
        };
        file.lines.push(line);
    }
    close(&mut current, &mut files);
    files
}

/// `@@ -12,7 +12,8 @@` → (12, 12)
fn hunk_starts(header: &str) -> (u32, u32) {
    let start = |prefix: char| {
        header
            .split(' ')
            .find_map(|part| part.strip_prefix(prefix))
            .and_then(|part| part.split(',').next()?.parse().ok())
            .unwrap_or(1)
    };
    (start('-'), start('+'))
}

/// Each run of removed lines is paired, line by line, with the run of added lines right after it,
/// and the words between their common start and common end are what changed.
fn mark_changes(lines: &mut [Line]) {
    let mut index = 0;
    while index < lines.len() {
        if lines[index].op != Op::Removed {
            index += 1;
            continue;
        }
        let removed_start = index;
        while index < lines.len() && lines[index].op == Op::Removed {
            index += 1;
        }
        let added_start = index;
        while index < lines.len() && lines[index].op == Op::Added {
            index += 1;
        }
        for offset in 0..(added_start - removed_start).min(index - added_start) {
            let (removed, added) = (removed_start + offset, added_start + offset);
            if let Some((before, after)) = changed(&lines[removed].text, &lines[added].text) {
                lines[removed].changed = Some(before);
                lines[added].changed = Some(after);
            }
        }
    }
}

/// The changed bytes of each line, or none when the lines share no word at either end or one of
/// them is too long.
pub fn changed(before: &str, after: &str) -> Option<(Range<usize>, Range<usize>)> {
    if before == after
        || before.chars().count() > LONGEST_COMPARED_LINE
        || after.chars().count() > LONGEST_COMPARED_LINE
    {
        return None;
    }
    let (a, b) = (words(before), words(after));
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let shared = a[..prefix].concat().trim().len() + a[a.len() - suffix..].concat().trim().len();
    if shared == 0 {
        return None;
    }
    let range = |tokens: &[&str]| {
        let start: usize = tokens[..prefix].iter().map(|t| t.len()).sum();
        let end: usize = tokens[..tokens.len() - suffix].iter().map(|t| t.len()).sum();
        start..end
    };
    Some((range(&a), range(&b)))
}

/// Runs of letters, digits and `_` are one word; every other character is a word of its own.
fn words(text: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut start = None;
    for (at, c) in text.char_indices() {
        if c.is_alphanumeric() || c == '_' {
            start.get_or_insert(at);
            continue;
        }
        if let Some(from) = start.take() {
            out.push(&text[from..at]);
        }
        out.push(&text[at..at + c.len_utf8()]);
    }
    if let Some(from) = start {
        out.push(&text[from..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../../fixtures/diffs/pull-request.diff");

    fn file<'a>(files: &'a [File], path: &str) -> &'a File {
        files.iter().find(|file| file.path == path).unwrap_or_else(|| panic!("no {path}"))
    }

    fn changed_text(line: &Line) -> Option<&str> {
        line.changed.clone().map(|range| &line.text[range])
    }

    #[test]
    fn a_realistic_diff_reads_as_files_with_status_old_path_and_binary() {
        let files = parse(FIXTURE);
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, [
            "Sources/ScheduleEditor.swift", "Sources/CompactRow.swift", "Sources/LongForm.swift",
            "Sources/Launch/Launch.swift", "Sources/Preferences.swift", "Assets/icon.png",
        ]);
        let status: Vec<_> = files.iter().map(|file| file.status).collect();
        assert_eq!(status, [
            Status::Modified, Status::Added, Status::Deleted, Status::Renamed, Status::Renamed, Status::Modified,
        ]);
        let old: Vec<_> = files.iter().map(|file| file.old_path.as_deref()).collect();
        assert_eq!(old, [None, None, None, Some("Old/Launch.swift"), Some("Sources/Settings.swift"), None]);
        let binary: Vec<_> = files.iter().map(|file| file.binary).collect();
        assert_eq!(binary, [false, false, false, false, false, true]);
        assert_eq!(files.iter().map(|file| file.status.badge(file.binary)).collect::<String>(), "MADRRB");
    }

    #[test]
    fn hunks_number_old_and_new_lines_and_keep_a_removed_line_that_starts_with_dashes() {
        let files = parse(FIXTURE);
        let editor = file(&files, "Sources/ScheduleEditor.swift");
        assert_eq!((editor.added(), editor.removed()), (3, 3));
        let number = |n: Option<u32>| n.map_or("_".to_owned(), |n| n.to_string());
        let numbered: Vec<_> = editor.lines.iter().map(|line| format!("{} {}", number(line.old), number(line.new))).collect();
        assert_eq!(numbered, ["_ _", "10 10", "11 _", "_ 11", "_ 12", "12 13", "13 14", "14 _", "_ _", "40 _", "_ 40", "41 41"]);
        assert!(editor.lines.iter().any(|line| line.op == Op::Removed && line.text == "--- old comment"));
        let hunks: Vec<_> = editor.lines.iter().filter(|line| line.op == Op::Hunk).map(|line| line.text.as_str()).collect();
        assert_eq!(hunks, ["@@ -10,5 +10,5 @@ struct ScheduleEditor: View {", "@@ -40,2 +40,2 @@ extension ScheduleEditor {"]);
        let long_form = file(&files, "Sources/LongForm.swift");
        assert_eq!(long_form.lines.last().map(|line| (line.op, line.text.as_str())), Some((Op::Note, "No newline at end of file")));
    }

    #[test]
    fn binaries_pure_renames_generated_and_large_files_start_folded() {
        let files = parse(FIXTURE);
        assert!(file(&files, "Assets/icon.png").starts_folded());
        assert!(file(&files, "Sources/Launch/Launch.swift").starts_folded());
        assert!(!file(&files, "Sources/LongForm.swift").starts_folded());
        assert!(!file(&files, "Sources/ScheduleEditor.swift").starts_folded());
        let lock = parse("diff --git a/Cargo.lock b/Cargo.lock\n@@ -1 +1 @@\n-a\n+b\n");
        assert!(lock[0].generated() && lock[0].starts_folded());
        let lines: String = (0..=FOLDED_CHANGE_THRESHOLD).map(|n| format!("+line {n}\n")).collect();
        let large = parse(&format!("diff --git a/big.txt b/big.txt\nnew file mode 100644\n@@ -0,0 +1,401 @@\n{lines}"));
        assert!(large[0].large() && large[0].starts_folded());
    }

    #[test]
    fn paired_removed_and_added_lines_mark_the_words_that_changed() {
        let files = parse(FIXTURE);
        let editor = file(&files, "Sources/ScheduleEditor.swift");
        let find = |text: &str| editor.lines.iter().find(|line| line.text == text).unwrap();
        assert_eq!(changed_text(find("        Form { longRows }")), Some("longRows"));
        assert_eq!(changed_text(find("        Form { compactRow }")), Some("compactRow"));
        assert_eq!(changed_text(find("    let title = \"Schedule\"")), Some("Schedule"));
        assert_eq!(changed_text(find("    let title = \"Repeat\"")), Some("Repeat"));
        assert_eq!(changed_text(find("            .sheet(isPresented: $showsLaunch) { LaunchOptions() }")), None);
        assert_eq!(changed("    return a", "    print(b)"), None);
        assert_eq!(changed("same", "same"), None);
        let long = "x".repeat(LONGEST_COMPARED_LINE);
        assert_eq!(changed(&format!("{long}a"), &format!("{long}b")), None);
        assert_eq!(changed("let año = 1", "let año = 2"), Some((11..12, 11..12)));
    }
}
