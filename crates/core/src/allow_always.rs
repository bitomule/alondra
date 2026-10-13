//! «Allow always» for a node's command prompt: the Claude Code rules that make this same command
//! stop asking, one `Bash(<program> <subcommand> *)` per part of a compound, or why there are none.
//! A command is read conservatively: anything whose effect its words do not show, anything that
//! deletes, overwrites, runs what it is given or reaches outside the worktree keeps asking.
//! `risks` is the same reading for the card's destructive mark, so the two never disagree.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MAX_RULES: usize = 5;
pub const MAX_COMMAND_CHARS: usize = 10_000;
pub const TOOL: &str = "Bash";

/// The permission mode the session runs in, from the hook's `permission_mode`. Only the modes
/// that approve writes in the working directory let a redirection to a file through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Manual,
    AcceptEdits,
    Auto,
    Other,
}
impl Mode {
    pub fn parse(mode: Option<&str>) -> Mode {
        match mode {
            Some("default") => Mode::Manual,
            Some("acceptEdits") => Mode::AcceptEdits,
            Some("auto") => Mode::Auto,
            _ => Mode::Other,
        }
    }
    fn writes_approved(self) -> bool {
        matches!(self, Mode::AcceptEdits | Mode::Auto)
    }
}

/// Where the command runs: the session's directory, the worktree it belongs to and Alondra's
/// files folder, which nodes may name in arguments.
#[derive(Clone, Copy, Debug)]
pub struct Place<'a> {
    pub cwd: &'a Path,
    pub worktree: &'a Path,
    pub files: &'a Path,
    pub mode: Mode,
}

/// Why a command gets no «allow always».
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    Deletes,
    Git,
    RunsAnything,
    System,
    Alondra,
    Outside,
    Unreadable,
    NoSubcommand,
    ChangesDirectory,
    WritesFile,
    AutoIgnores,
    Forced,
    TooMany,
    TooLong,
    AlreadyAllowed,
    Repo,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    /// The part of the command that keeps asking, as the user wrote it; empty when the reason is
    /// the whole command.
    pub part: String,
    pub why: Why,
}
impl Refusal {
    pub fn new(part: impl Into<String>, why: Why) -> Self {
        Refusal { part: part.into(), why }
    }
}

/// What a permission card offers beyond allowing once: the rules «allow always» adds to `repo`,
/// or why it is not offered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AllowAlways {
    Offered {
        repo: String,
        rules: Vec<String>,
        /// The parts that never asked, `tail` or `git status`, so the card can say so.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        quiet: Vec<String>,
    },
    Refused { part: String, why: Why },
}
impl AllowAlways {
    pub fn rules(&self) -> Option<&[String]> {
        match self {
            AllowAlways::Offered { rules, .. } => Some(rules),
            AllowAlways::Refused { .. } => None,
        }
    }
}
impl From<Refusal> for AllowAlways {
    fn from(refusal: Refusal) -> Self {
        AllowAlways::Refused { part: refusal.part, why: refusal.why }
    }
}

/// The permission update Claude Code applies for these rules: kept in the repo's local settings.
pub fn permission_update(rules: &[String]) -> serde_json::Value {
    serde_json::json!({
        "type": "addRules", "behavior": "allow", "destination": "localSettings",
        "rules": rules.iter().map(|rule| serde_json::json!({"toolName": TOOL, "ruleContent": rule})).collect::<Vec<_>>(),
    })
}

/// A rule as it is written in the settings file: `Bash(make build *)`.
pub fn settings_entry(rule: &str) -> String {
    format!("{TOOL}({rule})")
}

/// The command the rule lets through, without the trailing wildcard: `make build`.
pub fn rule_command(rule: &str) -> &str {
    rule.strip_suffix(" *").unwrap_or(rule)
}

/// The rules that stop this command from asking, sorted and without repeats, or why there are
/// none. Claude Code's own suggestions are not trusted: the rules are always rebuilt from the
/// command, and `suggestions_pass` says whether Claude Code's would have been acceptable.
pub fn rules_for(command: &str, place: &Place) -> Result<Vec<String>, Refusal> {
    if command.chars().count() > MAX_COMMAND_CHARS {
        return Err(Refusal::new("", Why::TooLong));
    }
    if let Some(risk) = risks(command).into_iter().next() {
        return Err(Refusal::new(risk.part, risk.kind.why()));
    }
    let parts = parse(command).ok_or_else(|| Refusal::new("", Why::Unreadable))?;
    if parts.is_empty() {
        return Err(Refusal::new("", Why::Unreadable));
    }
    let mut base = place.cwd.to_path_buf();
    let mut moved = false;
    let has_git = parts.iter().any(|part| strip(&part.words).ok().and_then(|w| w.first()).is_some_and(|w| w.text == "git"));
    let mut rules = BTreeSet::new();
    let mut unknown = None;
    for part in &parts {
        let shown = part.shown();
        let words = strip(&part.words).map_err(|why| Refusal::new(&shown, why))?;
        let program = words.first().ok_or_else(|| Refusal::new(&shown, Why::Unreadable))?;
        if program.glob {
            return Err(Refusal::new(&shown, Why::Unreadable));
        }
        if program.text == "cd" {
            if moved || has_git {
                return Err(Refusal::new(&shown, Why::ChangesDirectory));
            }
            let [_, target] = words else { return Err(Refusal::new(&shown, Why::ChangesDirectory)) };
            if target.glob || target.text == "-" {
                return Err(Refusal::new(&shown, Why::ChangesDirectory));
            }
            let to = lexical(&base, &target.text);
            if !inside(&to, place.worktree) {
                return Err(Refusal::new(&shown, Why::Outside));
            }
            check_redirects(part, place, &base, moved).map_err(|why| Refusal::new(&shown, why))?;
            base = to;
            moved = true;
            continue;
        }
        if let Some(why) = excluded(words, place.mode) {
            return Err(Refusal::new(&shown, why));
        }
        if words[1..].iter().any(|word| !argument_inside(&word.text, &base, place)) {
            return Err(Refusal::new(&shown, Why::Outside));
        }
        check_redirects(part, place, &base, moved).map_err(|why| Refusal::new(&shown, why))?;
        if program.text == "tee" {
            for target in words[1..].iter().filter(|w| !w.text.starts_with('-')) {
                check_write(&target.text, target.glob, place, &base, moved).map_err(|why| Refusal::new(&shown, why))?;
            }
            continue;
        }
        if read_only(words) {
            continue;
        }
        match rule_of(words) {
            Some(rule) => {
                rules.insert(rule);
            }
            None => {
                unknown.get_or_insert_with(|| Refusal::new(&shown, Why::NoSubcommand));
            }
        }
    }
    if let Some(refusal) = unknown {
        return Err(refusal);
    }
    if rules.len() > MAX_RULES {
        return Err(Refusal::new("", Why::TooMany));
    }
    if rules.is_empty() {
        return Err(Refusal::new("", Why::NoSubcommand));
    }
    Ok(rules.into_iter().collect())
}

/// The rules «allow always» offers for this prompt. Alondra's own, or, when the only thing it
/// cannot read is a part without a subcommand, Claude Code's suggestion if every one of its rules
/// is one Alondra would build: Claude Code knows more read-only commands, and suggests rules only
/// for the parts that ask. A suggestion that may have been cut to Claude Code's limit is not
/// trusted to cover the whole command.
pub fn offer(command: &str, place: &Place, suggestions: &serde_json::Value) -> Result<Vec<String>, Refusal> {
    match rules_for(command, place) {
        Err(refusal) if refusal.why == Why::NoSubcommand => claude_code_rules(suggestions, place.mode).ok_or(refusal),
        other => other,
    }
}

/// The read-only parts of a command, by name (`tail`, `git status`), in order and without repeats.
pub fn quiet_parts(command: &str) -> Vec<String> {
    let mut quiet: Vec<String> = vec![];
    for part in parse(command).unwrap_or_default() {
        let Ok(words) = strip(&part.words) else { continue };
        if !read_only(words) {
            continue;
        }
        let name = if words[0].text == "git" { format!("git {}", words[1].text) } else { words[0].text.clone() };
        if !quiet.contains(&name) {
            quiet.push(name);
        }
    }
    quiet
}

fn claude_code_rules(suggestions: &serde_json::Value, mode: Mode) -> Option<Vec<String>> {
    let text = |entry: &serde_json::Value, key: &str| entry.get(key).and_then(serde_json::Value::as_str).map(str::to_owned);
    let adds: Vec<&serde_json::Value> = suggestions.as_array()?.iter()
        .filter(|entry| text(entry, "type").as_deref() == Some("addRules") && text(entry, "behavior").as_deref() == Some("allow"))
        .collect();
    if adds.is_empty() || !adds.iter().all(|entry| suggestion_passes(entry, mode)) {
        return None;
    }
    let rules: BTreeSet<String> = adds.iter()
        .flat_map(|entry| entry["rules"].as_array().into_iter().flatten())
        .filter_map(|rule| rule.get("ruleContent").and_then(serde_json::Value::as_str).map(str::to_owned))
        .collect();
    (!rules.is_empty() && rules.len() < MAX_RULES).then(|| rules.into_iter().collect())
}

/// Whether every rule of a Claude Code suggestion is one Alondra would build itself: program and
/// subcommand followed by ` *`, nothing excluded. A suggestion that fails is dropped whole.
pub fn suggestion_passes(suggestion: &serde_json::Value, mode: Mode) -> bool {
    let Some(rules) = suggestion.get("rules").and_then(serde_json::Value::as_array) else { return false };
    !rules.is_empty() && rules.iter().all(|rule| {
        rule.get("toolName").and_then(serde_json::Value::as_str) == Some(TOOL)
            && rule.get("ruleContent").and_then(serde_json::Value::as_str).is_some_and(|content| rule_passes(content, mode))
    })
}

/// A rule's text is exactly `<program> <subcommand> *` for a program and subcommand the builder
/// would allow.
pub fn rule_passes(content: &str, mode: Mode) -> bool {
    let Some(head) = content.strip_suffix(" *") else { return false };
    let mut parts = head.split(' ');
    let (Some(program), Some(sub), None) = (parts.next(), parts.next(), parts.next()) else { return false };
    if !plain_program(program) || !subcommand(sub) {
        return false;
    }
    let words = [Word::plain(program), Word::plain(sub)];
    excluded(&words, mode).is_none() && !read_only(&words) && rule_of(&words).as_deref() == Some(content)
}

fn rule_of(words: &[Word]) -> Option<String> {
    let [program, sub, ..] = words else { return None };
    (plain_program(&program.text) && !sub.glob && subcommand(&sub.text)).then(|| format!("{} {} *", program.text, sub.text))
}

fn plain_program(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+')) && !text.starts_with(['-', '.'])
}

fn subcommand(text: &str) -> bool {
    text.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
}

// ---------------------------------------------------------------------------------------------
// Reading the command.

#[derive(Clone, Debug, PartialEq, Eq)]
struct Word {
    text: String,
    glob: bool,
}
impl Word {
    fn plain(text: &str) -> Word {
        Word { text: text.into(), glob: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Read,
    Write,
}

#[derive(Debug, Default)]
struct Part {
    words: Vec<Word>,
    redirects: Vec<(Direction, Word)>,
}
impl Part {
    fn shown(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    }
}

/// Splits at `&&`, `||`, `;`, `|`, `|&` and newlines, honouring quotes, and separates
/// redirections from the words. `None` for anything whose effect the words do not show:
/// expansions, substitutions, subshells, groups, background jobs, here-documents, comments, a
/// `~` path or a dangling operator.
fn parse(command: &str) -> Option<Vec<Part>> {
    let mut parts = vec![];
    let mut part = Part::default();
    let mut word: Option<Word> = None;
    let mut pending: Option<Direction> = None;
    let mut dangling = false;
    let mut chars = command.chars().peekable();

    fn push(word: &mut Option<Word>, c: char, glob: bool) {
        let w = word.get_or_insert(Word { text: String::new(), glob: false });
        w.text.push(c);
        w.glob |= glob;
    }
    fn finish(part: &mut Part, word: &mut Option<Word>, pending: &mut Option<Direction>) {
        if let Some(w) = word.take() {
            match pending.take() {
                Some(direction) => part.redirects.push((direction, w)),
                None => part.words.push(w),
            }
        }
    }
    fn fd_prefix(word: &mut Option<Word>) -> bool {
        if word.as_ref().is_some_and(|w| !w.text.is_empty() && w.text.chars().all(|c| c.is_ascii_digit())) {
            *word = None;
            return true;
        }
        false
    }
    // `>&2`, `2>&1`, `>&-`: a descriptor duplication writes no file.
    fn duplicate(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<()> {
        let mut target = String::new();
        while let Some(c) = chars.next_if(|c| c.is_ascii_digit() || *c == '-') {
            target.push(c);
        }
        let ends = chars.peek().is_none_or(|c| matches!(c, ' ' | '\t' | '\n' | ';' | '|' | '&'));
        (ends && (target == "-" || (!target.is_empty() && target.chars().all(|c| c.is_ascii_digit())))).then_some(())
    }

    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => finish(&mut part, &mut word, &mut pending),
            ';' | '\n' => {
                finish(&mut part, &mut word, &mut pending);
                if pending.is_some() || (dangling && part.words.is_empty()) {
                    return None;
                }
                if !part.words.is_empty() || !part.redirects.is_empty() {
                    parts.push(std::mem::take(&mut part));
                }
                dangling = false;
            }
            '|' | '&' => {
                if c == '&' && chars.next_if_eq(&'>').is_some() {
                    finish(&mut part, &mut word, &mut pending);
                    chars.next_if_eq(&'>');
                    pending = Some(Direction::Write);
                    continue;
                }
                if c == '&' && chars.next_if_eq(&'&').is_none() {
                    return None;
                }
                if c == '|' && chars.next_if_eq(&'|').is_none() {
                    chars.next_if_eq(&'&');
                }
                finish(&mut part, &mut word, &mut pending);
                if pending.is_some() || part.words.is_empty() {
                    return None;
                }
                parts.push(std::mem::take(&mut part));
                dangling = true;
            }
            '>' | '<' => {
                if fd_prefix(&mut word) {
                    if pending.is_some() {
                        return None;
                    }
                } else {
                    finish(&mut part, &mut word, &mut pending);
                }
                if pending.is_some() {
                    return None;
                }
                if c == '<' && matches!(chars.peek(), Some('<' | '(')) {
                    return None;
                }
                if c == '>' && chars.peek() == Some(&'(') {
                    return None;
                }
                if c == '>' {
                    chars.next_if(|n| matches!(n, '>' | '|'));
                }
                if chars.next_if_eq(&'&').is_some() {
                    duplicate(&mut chars)?;
                    continue;
                }
                pending = Some(if c == '>' { Direction::Write } else { Direction::Read });
            }
            '\'' => {
                word.get_or_insert(Word { text: String::new(), glob: false });
                loop {
                    match chars.next()? {
                        '\'' => break,
                        q => push(&mut word, q, false),
                    }
                }
            }
            '"' => {
                word.get_or_insert(Word { text: String::new(), glob: false });
                loop {
                    match chars.next()? {
                        '"' => break,
                        '$' | '`' => return None,
                        '\\' => match chars.next()? {
                            e @ ('"' | '\\' | '$' | '`') => push(&mut word, e, false),
                            e => {
                                push(&mut word, '\\', false);
                                push(&mut word, e, false);
                            }
                        },
                        q => push(&mut word, q, false),
                    }
                }
            }
            '\\' => match chars.next()? {
                '\n' => {}
                e => push(&mut word, e, false),
            },
            '~' if word.as_ref().is_none_or(|w| w.text.is_empty() || w.text.ends_with(['=', ':'])) => return None,
            '$' | '`' | '(' | ')' | '{' | '}' | '#' | '!' => return None,
            '*' | '?' | '[' | ']' => push(&mut word, c, true),
            c => push(&mut word, c, false),
        }
        if word.is_some() {
            dangling = false;
        }
    }
    finish(&mut part, &mut word, &mut pending);
    if pending.is_some() {
        return None;
    }
    if !part.words.is_empty() {
        parts.push(part);
    } else if dangling || !part.redirects.is_empty() {
        return None;
    }
    Some(parts)
}

/// Claude Code's harmless environment variables, which it strips before matching a rule; any
/// other assignment makes the command one no rule can approve.
const HARMLESS_VARIABLES: &[&str] = &[
    "GOEXPERIMENT", "GOOS", "GOARCH", "CGO_ENABLED", "GO111MODULE", "RUST_BACKTRACE", "RUST_LOG", "NODE_ENV",
    "PYTHONUNBUFFERED", "PYTHONDONTWRITEBYTECODE", "PYTEST_DISABLE_PLUGIN_AUTOLOAD", "PYTEST_DEBUG", "ANTHROPIC_API_KEY",
    "LANG", "LANGUAGE", "LC_ALL", "LC_CTYPE", "LC_TIME", "CHARSET", "TERM", "COLORTERM", "NO_COLOR", "FORCE_COLOR", "TZ",
    "LS_COLORS", "LSCOLORS", "GREP_COLOR", "GREP_COLORS", "GCC_COLORS", "TIME_STYLE", "BLOCK_SIZE", "BLOCKSIZE", "COLUMNS",
    "LINES", "CLICOLOR", "CLICOLOR_FORCE", "CI", "DEBIAN_FRONTEND", "GIT_TERMINAL_PROMPT",
];

/// The words Claude Code matches a rule against: without harmless variables in front and without
/// the wrappers it strips (`timeout 60`, `time`, `nice`, `nohup`, `stdbuf`, `command`, `builtin`,
/// `noglob`).
fn strip(words: &[Word]) -> Result<&[Word], Why> {
    let mut words = words;
    while let Some(name) = words.first().and_then(|w| assignment(&w.text)) {
        if !HARMLESS_VARIABLES.contains(&name) || words[0].glob {
            return Err(Why::Unreadable);
        }
        words = &words[1..];
    }
    loop {
        let Some(first) = words.first() else { return Err(Why::Unreadable) };
        let skip = match first.text.as_str() {
            "time" | "nohup" | "command" | "builtin" | "noglob" => 1,
            "timeout" => timeout_words(words).ok_or(Why::Unreadable)?,
            "nice" => match words.get(1).map(|w| w.text.as_str()) {
                Some("-n") => 3,
                Some(n) if n.starts_with("--adjustment=") || (n.starts_with('-') && n[1..].parse::<i32>().is_ok()) => 2,
                _ => 1,
            },
            "stdbuf" => 1 + words[1..].iter().take_while(|w| {
                let t = w.text.as_str();
                (t.len() > 2 && (t.starts_with("-i") || t.starts_with("-o") || t.starts_with("-e")))
                    || t.starts_with("--input=") || t.starts_with("--output=") || t.starts_with("--error=")
            }).count(),
            _ => return Ok(words),
        };
        if skip >= words.len() {
            return Err(Why::Unreadable);
        }
        words = &words[skip..];
    }
}

fn assignment(text: &str) -> Option<&str> {
    let (name, _) = text.split_once('=')?;
    (!name.is_empty() && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(name)
}

fn timeout_words(words: &[Word]) -> Option<usize> {
    let mut at = 1;
    loop {
        let text = words.get(at)?.text.as_str();
        match text {
            "--foreground" | "--preserve-status" | "--verbose" => at += 1,
            "-k" | "-s" => at += 2,
            t if t.starts_with("--kill-after=") || t.starts_with("--signal=") => at += 1,
            t => {
                let number = t.trim_end_matches(['s', 'm', 'h', 'd']);
                return (!number.is_empty() && number.parse::<f64>().is_ok()).then_some(at + 1);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// What always asks.

/// Programs that delete or overwrite whatever they are given.
const OVERWRITES: &[&str] = &[
    "rm", "rmdir", "unlink", "shred", "truncate", "dd", "mv", "cp", "ln", "install", "rsync", "chmod", "chown", "chgrp",
    "chflags", "xattr", "srm",
];
/// Subcommands that delete, under any program (`bazel clean`, `cargo clean`, `docker rm`).
const DELETING_SUBCOMMANDS: &[&str] = &[
    "clean", "purge", "prune", "expunge", "nuke", "reset", "destroy", "uninstall", "remove", "rm", "delete",
];
/// git subcommands that turn destructive with the right options; never allowed by rule.
const GIT_EXCLUDED: &[&str] = &[
    "push", "reset", "clean", "branch", "checkout", "restore", "switch", "stash", "worktree", "rebase", "tag", "update-ref",
    "filter-branch", "filter-repo", "reflog", "gc", "prune", "rm", "mv", "apply", "am", "config",
];
/// Programs that run whatever they are given.
const RUNS_ANYTHING: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ksh", "mksh", "fish", "csh", "tcsh", "rbash", "eval", "exec", "source", ".", "xargs", "env",
    "sudo", "su", "doas", "runuser", "ssh", "watch", "setsid", "flock", "ionice", "chrt", "taskset", "script", "unshare",
    "nsenter", "strace", "ltrace", "busybox", "node", "deno", "tsx", "ruby", "irb", "perl", "php", "lua", "osascript",
    "npx", "bunx", "uvx", "docker", "docker-compose", "podman", "devbox", "mise", "direnv", "nix", "nix-shell", "pwsh",
    "powershell", "parallel", "expect", "trap", "alias",
];
/// Subcommands that run whatever they are given.
const RUNS_ANYTHING_SUBCOMMANDS: &[(&str, &str)] = &[
    ("pnpm", "dlx"), ("pnpm", "exec"), ("npm", "exec"), ("yarn", "dlx"), ("yarn", "exec"), ("pipx", "run"), ("uv", "run"),
    ("poetry", "run"), ("bun", "x"), ("kubectl", "exec"), ("kubectl", "run"), ("kubectl", "debug"), ("kubectl", "attach"),
    ("bun", "exec"),
];
/// Subcommands auto mode drops from allow rules: package managers' `run`.
const AUTO_IGNORES: &[(&str, &str)] = &[("npm", "run"), ("yarn", "run"), ("pnpm", "run"), ("bun", "run")];
/// Programs that act on the Mac, the network or beyond the repo.
const SYSTEM: &[&str] = &[
    "launchctl", "defaults", "brew", "softwareupdate", "killall", "pkill", "kill", "open", "curl", "wget", "scp", "sftp",
    "ftp", "nc", "netcat", "telnet", "shutdown", "reboot", "halt", "diskutil", "networksetup", "scutil", "pmset", "security",
    "crontab", "systemctl", "sysctl", "mount", "umount", "kubectl", "aws", "gcloud", "gsutil", "az", "terraform", "helm",
    "vercel", "flyctl", "heroku", "csrutil", "spctl", "tccutil", "dscl", "sqlite3",
];
const SYSTEM_SUBCOMMANDS: &[(&str, &str)] = &[
    ("npm", "publish"), ("cargo", "publish"), ("yarn", "publish"), ("pnpm", "publish"), ("gh", "release"), ("gh", "repo"),
    ("gh", "api"), ("gh", "secret"), ("gh", "auth"), ("gh", "ssh-key"), ("gh", "gpg-key"), ("gh", "variable"),
    ("npm", "login"), ("npm", "adduser"), ("pod", "trunk"), ("cargo", "install"),
];
const ALONDRA: &[&str] = &["alondra", "alondrad"];
const SHELL_KEYWORDS: &[&str] = &[
    "for", "while", "until", "if", "then", "else", "elif", "fi", "do", "done", "case", "esac", "function", "select", "[",
    "[[", "test", "export", "unset", "set", "declare", "local", "readonly", "let",
];
/// Commands Claude Code treats as read-only: they need no rule.
const READ_ONLY: &[&str] = &["ls", "cat", "echo", "pwd", "head", "tail", "grep", "find", "wc", "which", "diff", "stat", "du"];
const READ_ONLY_GIT: &[&str] = &["status", "log", "diff", "show", "rev-parse"];
const FIND_ACTIONS: &[&str] = &["-exec", "-execdir", "-ok", "-okdir", "-delete", "-fprint", "-fprint0", "-fprintf", "-fls"];

fn base_name(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

fn interpreter(program: &str) -> bool {
    program.starts_with("python")
}

/// Why this part may never be allowed by rule, decided by its program and subcommand, not by the
/// options of this one call: the rule will cover the options of the next.
fn excluded(words: &[Word], mode: Mode) -> Option<Why> {
    let program = words.first()?.text.as_str();
    let sub = words.get(1).map(|w| w.text.as_str()).unwrap_or_default();
    let name = base_name(program);
    if SHELL_KEYWORDS.contains(&program) {
        return Some(Why::Unreadable);
    }
    if ALONDRA.contains(&name) {
        return Some(Why::Alondra);
    }
    if OVERWRITES.contains(&name) || (name == "sed" && words.iter().any(|w| w.text.starts_with("-i") || w.text.starts_with("--in-place"))) {
        return Some(Why::Deletes);
    }
    if name == "find" {
        if words.iter().any(|w| w.text == "-delete") {
            return Some(Why::Deletes);
        }
        if words.iter().any(|w| FIND_ACTIONS.contains(&w.text.as_str())) {
            return Some(Why::RunsAnything);
        }
    }
    if name == "git" {
        if sub == "-C" || sub == "-c" || words.iter().any(|w| w.text.starts_with("--git-dir") || w.text.starts_with("--work-tree") || w.text.starts_with("--exec-path")) {
            return Some(Why::Git);
        }
        if GIT_EXCLUDED.contains(&sub) {
            return Some(Why::Git);
        }
        if READ_ONLY_GIT.contains(&sub) && words.iter().any(|w| w.text.starts_with("--output") || w.text.starts_with("--ext-diff")) {
            return Some(Why::Git);
        }
    }
    if RUNS_ANYTHING.contains(&name) || interpreter(name) || (name == "swift" && !matches!(sub, "build" | "test")) {
        return Some(Why::RunsAnything);
    }
    if RUNS_ANYTHING_SUBCOMMANDS.contains(&(name, sub)) {
        return Some(Why::RunsAnything);
    }
    if SYSTEM.contains(&name) || SYSTEM_SUBCOMMANDS.contains(&(name, sub)) {
        return Some(Why::System);
    }
    if DELETING_SUBCOMMANDS.contains(&sub) {
        return Some(Why::Deletes);
    }
    if mode == Mode::Auto && AUTO_IGNORES.contains(&(name, sub)) {
        return Some(Why::AutoIgnores);
    }
    None
}

fn read_only(words: &[Word]) -> bool {
    match words {
        [program, rest @ ..] if program.text == "git" => rest.first().is_some_and(|sub| READ_ONLY_GIT.contains(&sub.text.as_str())),
        [program, ..] => READ_ONLY.contains(&program.text.as_str()),
        [] => false,
    }
}

// ---------------------------------------------------------------------------------------------
// Paths.

/// `path` resolved against `base` without touching the disk: `..` climbs, `.` stays.
fn lexical(base: &Path, path: &str) -> PathBuf {
    let joined = if Path::new(path).is_absolute() { PathBuf::from(path) } else { base.join(path) };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn inside(path: &Path, folder: &Path) -> bool {
    path.starts_with(folder)
}

/// An argument that names a path stays in the worktree or in Alondra's files folder: absolute
/// paths and `..` are checked, also after `--flag=`. A Bazel label (`//crates/...`) is no path.
fn argument_inside(text: &str, base: &Path, place: &Place) -> bool {
    let candidates = [Some(text), text.split_once('=').map(|(_, value)| value)];
    candidates.into_iter().flatten().all(|candidate| {
        let climbs = Path::new(candidate).components().any(|c| c == Component::ParentDir);
        let label = candidate.starts_with("//") && !climbs;
        if label || (!candidate.starts_with('/') && !climbs) {
            return true;
        }
        let to = lexical(base, candidate);
        inside(&to, place.worktree) || inside(&to, place.files)
    })
}

fn check_redirects(part: &Part, place: &Place, base: &Path, moved: bool) -> Result<(), Why> {
    for (direction, target) in &part.redirects {
        match direction {
            Direction::Write => check_write(&target.text, target.glob, place, base, moved)?,
            Direction::Read => {
                if target.glob || (moved && !target.text.starts_with('/')) {
                    return Err(Why::Outside);
                }
                if !inside(&lexical(base, &target.text), place.worktree) {
                    return Err(Why::Outside);
                }
            }
        }
    }
    Ok(())
}

/// A file write keeps asking in manual mode even with the rule, and always outside the worktree.
fn check_write(target: &str, glob: bool, place: &Place, base: &Path, moved: bool) -> Result<(), Why> {
    if target == "/dev/null" {
        return Ok(());
    }
    if glob {
        return Err(Why::WritesFile);
    }
    if moved && !target.starts_with('/') {
        return Err(Why::ChangesDirectory);
    }
    if !inside(&lexical(base, target), place.worktree) {
        return Err(Why::Outside);
    }
    if !place.mode.writes_approved() {
        return Err(Why::WritesFile);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The destructive mark.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RiskKind {
    PushForce,
    WorktreeRemoveForce,
    BranchForceDelete,
    ResetHard,
    CleanForce,
    DiscardsChanges,
    RewritesHistory,
    Deletes,
    Overwrites,
    Force,
    ForcedVariant,
}
impl RiskKind {
    fn why(self) -> Why {
        match self {
            RiskKind::Deletes | RiskKind::Overwrites => Why::Deletes,
            RiskKind::Force | RiskKind::ForcedVariant => Why::Forced,
            _ => Why::Git,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Risk {
    pub kind: RiskKind,
    /// The part of the command that carries it.
    pub part: String,
}

/// What each part of a command can destroy, one entry per kind of risk, read segment by segment
/// even through subshells and substitutions, so `cd x && rm y` and `echo $(rm y)` are both caught.
/// Everything this marks, `rules_for` refuses.
pub fn risks(command: &str) -> Vec<Risk> {
    let mut found: Vec<Risk> = vec![];
    for segment in command.split(|c: char| matches!(c, ';' | '&' | '|' | '(' | ')' | '`' | '\n' | '{' | '}')) {
        let all: Vec<String> = segment.split_whitespace().map(|w| w.trim_matches(['\'', '"']).to_owned()).collect();
        let words = unwrapped(&all);
        let kind = risk_of(words);
        if let Some(kind) = kind.filter(|kind| !found.iter().any(|r| r.kind == *kind)) {
            found.push(Risk { kind, part: words.join(" ") });
        }
    }
    found
}

fn risk_of(words: &[String]) -> Option<RiskKind> {
    if base_name(words.first()?) == "find" {
        if let Some(at) = words.iter().position(|w| matches!(w.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir")) {
            if let Some(kind) = risk_of(unwrapped(&words[at + 1..])) {
                return Some(kind);
            }
        }
    }
    let program = base_name(words.first()?);
    let has = |w: &str| words.iter().any(|x| x == w);
    let force = words.iter().any(|w| w.starts_with("--force"));
    let short_flag_with = |f: char| words.iter().any(|w| w.starts_with('-') && !w.starts_with("--") && w.contains(f));
    let git = program == "git";
    let sub = git_subcommand(words);
    if git && sub == Some("push") && (force || short_flag_with('f')) {
        Some(RiskKind::PushForce)
    } else if git && sub == Some("worktree") && has("remove") && (force || short_flag_with('f')) {
        Some(RiskKind::WorktreeRemoveForce)
    } else if git && sub == Some("branch") && (has("-D") || ((has("-d") || has("--delete")) && (force || short_flag_with('f')))) {
        Some(RiskKind::BranchForceDelete)
    } else if git && sub == Some("reset") && (has("--hard") || has("--merge") || has("--keep")) {
        Some(RiskKind::ResetHard)
    } else if git && sub == Some("clean") && (force || short_flag_with('f')) {
        Some(RiskKind::CleanForce)
    } else if git && (matches!(sub, Some("restore")) || (sub == Some("checkout") && (has("--") || has(".") || has("-f") || force))
        || (sub == Some("stash") && (has("drop") || has("clear")))) {
        Some(RiskKind::DiscardsChanges)
    } else if git && (matches!(sub, Some("filter-branch" | "filter-repo" | "update-ref")) || (sub == Some("reflog") && (has("expire") || has("delete")))) {
        Some(RiskKind::RewritesHistory)
    } else if !git && (matches!(program, "rm" | "rmdir" | "unlink" | "shred" | "truncate" | "srm")
        || (program == "find" && has("-delete"))
        || words.get(1).is_some_and(|s| DELETING_SUBCOMMANDS.contains(&s.as_str()) && plain_program(program))) {
        Some(RiskKind::Deletes)
    } else if !git && (OVERWRITES.contains(&program) || (program == "sed" && words.iter().any(|w| w.starts_with("-i") || w.starts_with("--in-place")))
        || (program == "perl" && words.iter().any(|w| w.starts_with("-i") || w.starts_with("-pi")))) {
        Some(RiskKind::Overwrites)
    } else if force {
        Some(RiskKind::Force)
    } else if has("-D") || has("--hard") {
        Some(RiskKind::ForcedVariant)
    } else {
        None
    }
}

/// The words of a segment without the variables, wrappers and runners in front of the command
/// they run: `sudo rm x`, `xargs -0 rm`, `timeout 5 rm x`, `env A=1 rm x`.
fn unwrapped(words: &[String]) -> &[String] {
    let mut words = words;
    loop {
        let Some(first) = words.first() else { return words };
        let name = base_name(first);
        let skip = if assignment(first).is_some() {
            1
        } else {
            match name {
                "time" | "nohup" | "command" | "builtin" | "noglob" | "exec" => 1,
                "sudo" | "doas" | "env" | "xargs" | "nice" | "stdbuf" | "timeout" => {
                    let options = words[1..].iter().take_while(|w| w.starts_with('-') || assignment(w).is_some()).count();
                    let duration = usize::from(name == "timeout" && words.get(1 + options).is_some_and(|w| w.trim_end_matches(['s', 'm', 'h', 'd']).parse::<f64>().is_ok()));
                    1 + options + duration
                }
                _ => return words,
            }
        };
        if skip >= words.len() {
            return &words[words.len()..];
        }
        words = &words[skip..];
    }
}

fn git_subcommand(words: &[String]) -> Option<&str> {
    let mut at = 1;
    while let Some(word) = words.get(at) {
        match word.as_str() {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" => at += 2,
            w if w.starts_with('-') => at += 1,
            w => return Some(w),
        }
    }
    None
}

#[cfg(test)]
#[path = "allow_always_tests.rs"]
mod tests;
