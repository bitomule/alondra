//! «Allow in this task» for a node's command prompt: what the user's yes keeps allowing until the
//! task closes, or why it cannot be offered. Only a command whose folder and paths stay in the
//! task's worktrees, Alondra's files folder or the sandbox's temporary folders is offered. A read,
//! a test or a build is allowed by its shape (the same program and subcommand); anything else,
//! above all what deletes or discards, only as the exact same command run from the same folder.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::allow_always::{self, Direction, Part, Word};

/// The temporary folders the sandbox's scripts make for their sockets and binaries.
const SANDBOX_TEMPORARY: &[&str] = &["asb.", "alondra-sbx-"];

const READS: &[&str] = &["ls", "cat", "head", "tail", "wc", "grep", "rg", "stat", "du", "diff", "pwd", "which", "file"];
const GIT_READS: &[&str] = &["status", "log", "diff", "show", "rev-parse", "ls-files", "blame"];
const BUILDS: &[(&[&str], &[&str])] = &[
    (&["bazel", "bazelisk"], &["build", "test", "query", "cquery", "aquery", "info"]),
    (&["cargo"], &["build", "test", "check", "clippy"]),
    (&["swift", "xcodebuild"], &["build", "test"]),
    (&["go"], &["build", "test", "vet"]),
];
const SCRIPTS: &[&str] = &["typecheck", "build", "test"];
/// Options that make a build or test run a program of the caller's choosing.
const RUNS_GIVEN: &[&str] = &["--run_under", "--script_path", "--config-file", "--exec"];

/// What «allow in this task» offers for a prompt, kept on its card so what the card shows is what
/// is granted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AllowForTask {
    /// This command, byte for byte, run from this folder.
    Exact { command: String, cwd: String },
    /// Any call of the program and subcommand in `shape` (`["bazelisk", "test"]`) whose folder and
    /// paths stay in the task's folders; `command` and `cwd` are the call that was asked.
    Shape { command: String, cwd: String, shape: Vec<String> },
    Refused {
        why: NotForTask,
        /// The folder or path that leaves the task's folders, or the part that cannot be read.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        part: String,
    },
}

/// Why a prompt gets no «allow in this task».
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotForTask {
    /// It is not a terminal command.
    NotCommand,
    /// Claude Code says this prompt must not get a permission that lasts.
    Persistent,
    /// The user's Claude Code settings ask for this command every time.
    AskRule,
    /// Alondra does not know the folder the command runs from.
    NoFolder,
    /// The folder or a path is outside the task's folders.
    Outside,
    /// Its words do not show what it touches: variables, substitutions, `~`.
    Unreadable,
    TooLong,
}

impl AllowForTask {
    pub fn offered(&self) -> bool {
        !matches!(self, AllowForTask::Refused { .. })
    }

    /// Whether this grant lets `asked` run without asking: the same command from the same folder,
    /// or, for a shape, a call of that shape. A prompt outside the task's folders is never covered.
    pub fn covers(&self, asked: &AllowForTask) -> bool {
        let (asked_command, asked_cwd, asked_shape) = match asked {
            AllowForTask::Exact { command, cwd } => (command, cwd, None),
            AllowForTask::Shape { command, cwd, shape } => (command, cwd, Some(shape)),
            AllowForTask::Refused { .. } => return false,
        };
        match self {
            AllowForTask::Exact { command, cwd } => command == asked_command && cwd == asked_cwd,
            AllowForTask::Shape { shape, .. } => asked_shape == Some(shape),
            AllowForTask::Refused { .. } => false,
        }
    }
}

/// A permission prompt as the SDK host reported it.
#[derive(Clone, Copy, Debug)]
pub struct Prompt<'a> {
    pub tool: Option<&'a str>,
    pub input: &'a serde_json::Value,
    /// The folder the command runs from, as Claude Code reported it before the call.
    pub cwd: Option<&'a str>,
    /// Claude Code's `suppressAlwaysAllowRule`.
    pub persistent_refused: bool,
    /// Claude Code's `matchedAskRule`.
    pub ask_rule: bool,
}

/// What «allow in this task» offers for `prompt` in a task whose folders are `folders`: its open
/// worktrees and Alondra's files folder. The sandbox's temporary folders always count.
pub fn assess(prompt: &Prompt, folders: &[PathBuf]) -> AllowForTask {
    let refused = |why: NotForTask, part: &str| AllowForTask::Refused { why, part: part.to_owned() };
    if prompt.tool != Some(allow_always::TOOL) {
        return refused(NotForTask::NotCommand, "");
    }
    if prompt.persistent_refused {
        return refused(NotForTask::Persistent, "");
    }
    if prompt.ask_rule {
        return refused(NotForTask::AskRule, "");
    }
    let Some(command) = prompt.input.get("command").and_then(serde_json::Value::as_str) else {
        return refused(NotForTask::Unreadable, "");
    };
    if command.chars().count() > allow_always::MAX_COMMAND_CHARS {
        return refused(NotForTask::TooLong, "");
    }
    let Some(cwd) = prompt.cwd.filter(|cwd| Path::new(cwd).is_absolute()) else {
        return refused(NotForTask::NoFolder, "");
    };
    let scope = Scope { folders };
    let base = allow_always::lexical(Path::new("/"), cwd);
    if !scope.holds(&base) {
        return refused(NotForTask::Outside, cwd);
    }
    let Some(parts) = allow_always::parse(command).filter(|parts| !parts.is_empty()) else {
        return refused(NotForTask::Unreadable, "");
    };
    if let Err(refusal) = scope.check(&parts, base) {
        return refusal;
    }
    let exact = || AllowForTask::Exact { command: command.to_owned(), cwd: cwd.to_owned() };
    let [part] = parts.as_slice() else { return exact() };
    if !allow_always::risks(command).is_empty() || part.redirects.iter().any(|(direction, target)| *direction == Direction::Write && target.text != "/dev/null") {
        return exact();
    }
    match allow_always::strip(&part.words).ok().and_then(shape) {
        Some(shape) => AllowForTask::Shape { command: command.to_owned(), cwd: cwd.to_owned(), shape },
        None => exact(),
    }
}

struct Scope<'a> {
    folders: &'a [PathBuf],
}

impl Scope<'_> {
    fn holds(&self, path: &Path) -> bool {
        self.folders.iter().any(|folder| path.starts_with(folder)) || sandbox_temporary(path)
    }

    /// Every folder the command moves to and every path it names stays in the task's folders.
    fn check(&self, parts: &[Part], mut base: PathBuf) -> Result<(), AllowForTask> {
        let outside = |part: &str| AllowForTask::Refused { why: NotForTask::Outside, part: part.to_owned() };
        let unreadable = |part: String| AllowForTask::Refused { why: NotForTask::Unreadable, part };
        for part in parts {
            let words = allow_always::strip(&part.words).map_err(|_| unreadable(part.shown()))?;
            let program = words.first().ok_or_else(|| unreadable(part.shown()))?;
            if program.glob {
                return Err(unreadable(part.shown()));
            }
            for (_, target) in &part.redirects {
                if target.text != "/dev/null" && !self.path_holds(target, &base) {
                    return Err(outside(&target.text));
                }
            }
            if program.text == "cd" {
                let [_, target] = words else { return Err(unreadable(part.shown())) };
                if target.glob || target.text == "-" {
                    return Err(unreadable(part.shown()));
                }
                let to = allow_always::lexical(&base, &target.text);
                if !self.holds(&to) {
                    return Err(outside(&target.text));
                }
                base = to;
                continue;
            }
            let skipped = part.words.len() - words.len();
            for (at, word) in part.words.iter().enumerate() {
                if at != skipped && !self.argument_holds(word, &base) {
                    return Err(outside(&word.text));
                }
            }
        }
        Ok(())
    }

    /// An argument that names a path, also after `--flag=`, stays in the task's folders. A Bazel
    /// label (`//crates/...`) is no path; a relative path that does not climb stays in `base`.
    fn argument_holds(&self, word: &Word, base: &Path) -> bool {
        let candidates = [Some(word.text.as_str()), word.text.split_once('=').map(|(_, value)| value)];
        candidates.into_iter().flatten().all(|candidate| {
            let climbs = Path::new(candidate).components().any(|c| c == Component::ParentDir);
            if (candidate.starts_with("//") || !candidate.starts_with('/')) && !climbs {
                return true;
            }
            self.path_holds(&Word { text: candidate.to_owned(), glob: word.glob }, base)
        })
    }

    /// A path, or for a pattern the folder before its first wildcard, stays in the task's folders.
    fn path_holds(&self, path: &Word, base: &Path) -> bool {
        let resolved = allow_always::lexical(base, &path.text);
        if !path.glob {
            return self.holds(&resolved);
        }
        let fixed: PathBuf = resolved.components().take_while(|c| !c.as_os_str().to_string_lossy().contains(['*', '?', '[', ']'])).collect();
        self.holds(&fixed)
    }
}

/// `/tmp/asb.x…` or `/private/tmp/alondra-sbx-…`: one of the sandbox's own temporary folders.
fn sandbox_temporary(path: &Path) -> bool {
    let names: Vec<String> = path.components().filter_map(|c| match c {
        Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
        _ => None,
    }).collect();
    let rest = match names.as_slice() {
        [private, tmp, rest @ ..] if private == "private" && tmp == "tmp" => rest,
        [tmp, rest @ ..] if tmp == "tmp" => rest,
        _ => return false,
    };
    rest.first().is_some_and(|name| SANDBOX_TEMPORARY.iter().any(|prefix| name.len() > prefix.len() && name.starts_with(prefix)))
}

/// The shape a read, a test or a build is allowed by: its program and subcommand.
fn shape(words: &[Word]) -> Option<Vec<String>> {
    let text = |at: usize| words.get(at).filter(|word| !word.glob).map(|word| word.text.as_str());
    let program = text(0).filter(|program| allow_always::plain_program(program))?;
    let has = |options: &[&str]| words.iter().any(|word| options.iter().any(|option| word.text.starts_with(option)));
    let of = |parts: &[&str]| Some(parts.iter().map(|part| (*part).to_owned()).collect());
    if READS.contains(&program) {
        return of(&[program]);
    }
    if program == "find" {
        return (!has(allow_always::FIND_ACTIONS) && !has(&["-delete"])).then(|| vec![program.to_owned()]);
    }
    if program == "pytest" {
        return of(&[program]);
    }
    let sub = text(1)?;
    if program == "git" {
        return (GIT_READS.contains(&sub) && !has(&["--output", "--ext-diff", "--textconv"])).then(|| vec![program.to_owned(), sub.to_owned()]);
    }
    if BUILDS.iter().any(|(programs, subs)| programs.contains(&program) && subs.contains(&sub)) {
        return (!has(RUNS_GIVEN)).then(|| vec![program.to_owned(), sub.to_owned()]);
    }
    if matches!(program, "npm" | "pnpm") {
        return match (sub, text(2)) {
            ("test", _) => of(&[program, sub]),
            ("run", Some(script)) if SCRIPTS.contains(&script) => of(&[program, sub, script]),
            _ => None,
        };
    }
    if matches!(program, "python" | "python3") && sub == "-m" {
        return text(2).filter(|module| matches!(*module, "unittest" | "pytest")).and_then(|module| of(&[program, sub, module]));
    }
    None
}

#[cfg(test)]
#[path = "allow_for_task_tests.rs"]
mod tests;
