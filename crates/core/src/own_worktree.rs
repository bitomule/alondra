//! Commands that only change files of the task's own worktrees, which Alondra allows without a
//! card: `rm` (also of a subfolder), `rmdir`, `unlink`, `mv`, `sed -i` with substitutions,
//! `truncate`, `chmod` and `git checkout`/`git restore` of paths. The decision reads the disk, not
//! the words: every path is resolved as the kernel resolves it, with its folder canonicalized
//! (symlinks and `..` followed) and its last name looked at without following it, unless the
//! operation writes through it. It must land strictly inside a linked worktree of the task, never
//! on its root, a `.git` or another worktree of the task. Anything it cannot read for certain keeps
//! asking, and so does every part of the command that is not one of these operations.
//!
//! The disk is read when the request arrives and the shell runs the command after Alondra answers:
//! a process of the same task could swap a folder for a symlink in between. Only Alondra running
//! the command itself would close that window; the check accepts it and never turns a failed read
//! into a permission.

use std::path::{Path, PathBuf};

use crate::allow_always::{self, Direction, Word};

/// Why a command keeps asking: what the check could not prove.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keeps {
    /// Its words do not show what it does: expansions, substitutions, a wrapper, too long.
    Unreadable,
    /// A part of it is not a file change the check knows.
    NotFileChange,
    /// A path, or the folder it runs from, is outside the task's own worktrees.
    Outside,
    /// A path is a worktree's root, a `.git`, or holds another worktree of the task.
    Protected,
    /// A path cannot be read for certain: missing folder, unreadable link, pattern, shared file.
    Uncertain,
}

/// The task's worktrees the check trusts, canonicalized: the recorded folders that are linked git
/// worktrees, whose `.git` is a file. A main checkout, the user's own folder, never counts.
pub fn roots<'a>(recorded: impl IntoIterator<Item = &'a str>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = recorded
        .into_iter()
        .filter_map(|path| {
            let real = std::fs::canonicalize(path).ok()?;
            std::fs::symlink_metadata(real.join(".git")).ok()?.is_file().then_some(real)
        })
        .collect();
    roots.sort();
    roots.dedup();
    roots
}

/// Whether `command`, run from `cwd`, only changes files inside `roots` (from `roots`).
pub fn judge(command: &str, cwd: &str, roots: &[PathBuf]) -> Result<(), Keeps> {
    if command.chars().count() > allow_always::MAX_COMMAND_CHARS {
        return Err(Keeps::Unreadable);
    }
    if roots.is_empty() || !Path::new(cwd).is_absolute() {
        return Err(Keeps::Outside);
    }
    let base = std::fs::canonicalize(cwd).map_err(|_| Keeps::Uncertain)?;
    let mut here = Here { roots, base };
    here.root_holding(&here.base).ok_or(Keeps::Outside)?;
    let parts = allow_always::parse(command).filter(|parts| !parts.is_empty()).ok_or(Keeps::Unreadable)?;
    for part in &parts {
        if part.redirects.iter().any(|(direction, target)| *direction != Direction::Write || target.glob || target.text != "/dev/null") {
            return Err(Keeps::NotFileChange);
        }
        let words = allow_always::strip(&part.words).map_err(|_| Keeps::Unreadable)?;
        let (program, args) = words.split_first().ok_or(Keeps::Unreadable)?;
        if program.glob || program.text.contains('/') {
            return Err(Keeps::NotFileChange);
        }
        match program.text.as_str() {
            "cd" => here.cd(args)?,
            "true" | ":" if args.is_empty() => {}
            "rm" => here.rm(args)?,
            "rmdir" => here.rmdir(args)?,
            "unlink" => match args {
                [target] => here.removed(target, false)?,
                _ => return Err(Keeps::NotFileChange),
            },
            "mv" => here.mv(args)?,
            "sed" => here.sed(args)?,
            "truncate" => here.truncate(args)?,
            "chmod" => here.chmod(args)?,
            "git" => here.git(args)?,
            _ => return Err(Keeps::NotFileChange),
        }
    }
    Ok(())
}

struct Here<'a> {
    roots: &'a [PathBuf],
    /// The real folder the next part runs from.
    base: PathBuf,
}

fn glob_char(c: char) -> bool {
    matches!(c, '*' | '?' | '[' | ']')
}

/// `text` split at its last `/` into the folder it names and its last name; the folder is empty
/// for a bare name and `/` for a name at the top.
fn split(text: &str) -> (&str, &str) {
    match text.rfind('/') {
        Some(0) => ("/", &text[1..]),
        Some(at) => (&text[..at], &text[at + 1..]),
        None => ("", text),
    }
}

impl Here<'_> {
    /// The innermost worktree holding `real`, itself included.
    fn root_holding(&self, real: &Path) -> Option<&Path> {
        self.roots.iter().filter(|root| real.starts_with(root)).max_by_key(|root| root.components().count()).map(PathBuf::as_path)
    }

    /// `real` is strictly inside a worktree, outside any `.git`, and holds no worktree.
    fn owned(&self, real: &Path) -> Result<(), Keeps> {
        let root = self.root_holding(real).ok_or(Keeps::Outside)?;
        let within = real.strip_prefix(root).map_err(|_| Keeps::Outside)?;
        if within.as_os_str().is_empty() || within.components().any(|c| c.as_os_str() == ".git") {
            return Err(Keeps::Protected);
        }
        if self.roots.iter().any(|other| other.starts_with(real)) {
            return Err(Keeps::Protected);
        }
        Ok(())
    }

    fn folder(&self, text: &str) -> Result<PathBuf, Keeps> {
        let joined = if text.is_empty() { self.base.clone() } else { self.base.join(text) };
        std::fs::canonicalize(joined).map_err(|_| Keeps::Uncertain)
    }

    /// The real path of an entry the operation removes or replaces without following it: its
    /// folder resolved, its last name as it is. A trailing `/` or `/.` makes the system follow the
    /// last name too, and so does the check.
    fn entry(&self, word: &Word) -> Result<PathBuf, Keeps> {
        let text = word.text.as_str();
        if word.glob || text.is_empty() {
            return Err(Keeps::Uncertain);
        }
        if text == "." || text.ends_with('/') || text.ends_with("/.") {
            return self.followed(word);
        }
        let (folder, name) = split(text);
        if matches!(name, "" | "." | "..") {
            return Err(Keeps::Uncertain);
        }
        Ok(self.folder(folder)?.join(name))
    }

    /// The real path of an entry the operation writes through: every link followed. A link whose
    /// target cannot be read is uncertain; a name that does not exist yet is created in its folder.
    fn followed(&self, word: &Word) -> Result<PathBuf, Keeps> {
        if word.glob || word.text.is_empty() {
            return Err(Keeps::Uncertain);
        }
        let joined = self.base.join(&word.text);
        match std::fs::canonicalize(&joined) {
            Ok(real) => Ok(real),
            Err(_) if std::fs::symlink_metadata(&joined).is_ok() => Err(Keeps::Uncertain),
            Err(_) => {
                let (folder, name) = split(&word.text);
                if matches!(name, "" | "." | "..") {
                    return Err(Keeps::Uncertain);
                }
                Ok(self.folder(folder)?.join(name))
            }
        }
    }

    /// An entry `rm`, `rmdir`, `unlink` or `mv` takes away; a pattern only in its last name, from
    /// a folder inside a worktree that is not its root and holds no `.git`.
    fn removed(&self, word: &Word, pattern_allowed: bool) -> Result<(), Keeps> {
        if !word.glob {
            return self.owned(&self.entry(word)?);
        }
        let (folder, name) = split(&word.text);
        if !pattern_allowed || folder.contains(glob_char) || name.is_empty() || name.starts_with('.') || name.starts_with(['?', '[']) {
            return Err(Keeps::Uncertain);
        }
        let real = self.folder(folder)?;
        self.owned(&real)?;
        if std::fs::symlink_metadata(real.join(".git")).is_ok() {
            return Err(Keeps::Protected);
        }
        Ok(())
    }

    /// A file written through every link: inside, and not shared with another name by a hard link
    /// when the operation changes it in place.
    fn written(&self, word: &Word, in_place: bool) -> Result<PathBuf, Keeps> {
        let real = self.followed(word)?;
        self.owned(&real)?;
        if in_place {
            if let Ok(metadata) = std::fs::metadata(&real) {
                use std::os::unix::fs::MetadataExt;
                if metadata.is_file() && metadata.nlink() > 1 {
                    return Err(Keeps::Uncertain);
                }
            }
        }
        Ok(real)
    }

    fn cd(&mut self, args: &[Word]) -> Result<(), Keeps> {
        let [target] = args else { return Err(Keeps::NotFileChange) };
        if target.glob || target.text.is_empty() || target.text == "-" {
            return Err(Keeps::Uncertain);
        }
        let real = self.folder(&target.text)?;
        let root = self.root_holding(&real).ok_or(Keeps::Outside)?;
        if real.strip_prefix(root).is_ok_and(|within| within.components().any(|c| c.as_os_str() == ".git")) {
            return Err(Keeps::Protected);
        }
        self.base = real;
        Ok(())
    }

    /// The words after the options, with the options checked against `known`.
    fn operands<'w>(args: &'w [Word], short: &str, long: &[&str]) -> Result<Vec<&'w Word>, Keeps> {
        let mut operands = vec![];
        let mut ended = false;
        for word in args {
            let text = word.text.as_str();
            if ended || word.glob || !text.starts_with('-') || text == "-" {
                operands.push(word);
            } else if text == "--" {
                ended = true;
            } else if text.starts_with("--") {
                if !long.contains(&text) {
                    return Err(Keeps::NotFileChange);
                }
            } else if !text[1..].chars().all(|c| short.contains(c)) {
                return Err(Keeps::NotFileChange);
            }
        }
        Ok(operands)
    }

    fn rm(&self, args: &[Word]) -> Result<(), Keeps> {
        let targets = Self::operands(args, "frRdviIP", &["--force", "--recursive", "--dir", "--verbose"])?;
        if targets.is_empty() {
            return Err(Keeps::NotFileChange);
        }
        targets.into_iter().try_for_each(|target| self.removed(target, true))
    }

    fn rmdir(&self, args: &[Word]) -> Result<(), Keeps> {
        let targets = Self::operands(args, "v", &["--verbose"])?;
        if targets.is_empty() {
            return Err(Keeps::NotFileChange);
        }
        targets.into_iter().try_for_each(|target| self.removed(target, false))
    }

    /// The sources go as `rm` takes them; a destination that is a folder is entered, with the
    /// worktree's root allowed, and anything else is replaced without following it.
    fn mv(&self, args: &[Word]) -> Result<(), Keeps> {
        let operands = Self::operands(args, "fniv", &["--force", "--no-clobber", "--verbose"])?;
        let [sources @ .., destination] = operands.as_slice() else { return Err(Keeps::NotFileChange) };
        if sources.is_empty() {
            return Err(Keeps::NotFileChange);
        }
        for source in sources {
            self.removed(source, false)?;
        }
        if destination.glob {
            return Err(Keeps::Uncertain);
        }
        let joined = self.base.join(&destination.text);
        if std::fs::metadata(&joined).is_ok_and(|m| m.is_dir()) {
            let real = std::fs::canonicalize(&joined).map_err(|_| Keeps::Uncertain)?;
            return match self.owned(&real) {
                Err(Keeps::Protected) if self.roots.iter().any(|root| *root == real) => Ok(()),
                other => other,
            };
        }
        self.removed(destination, false)
    }

    /// `sed -i` with scripts of substitutions and deletions only, on files that exist.
    fn sed(&self, args: &[Word]) -> Result<(), Keeps> {
        let (mut in_place, mut scripts, mut positional) = (false, Vec::<&str>::new(), Vec::<&Word>::new());
        let mut at = 0;
        let mut ended = false;
        while let Some(word) = args.get(at) {
            let text = word.text.as_str();
            at += 1;
            if ended || word.glob || !text.starts_with('-') {
                positional.push(word);
                continue;
            }
            match text {
                "--" => ended = true,
                "-i" => {
                    in_place = true;
                    if let Some(suffix) = args.get(at).filter(|next| !next.glob && (next.text.is_empty() || next.text.starts_with('.'))) {
                        if !backup_suffix(&suffix.text) {
                            return Err(Keeps::Uncertain);
                        }
                        at += 1;
                    }
                }
                "-e" => {
                    let script = args.get(at).filter(|next| !next.glob).ok_or(Keeps::Uncertain)?;
                    scripts.push(&script.text);
                    at += 1;
                }
                "-E" | "-r" | "-n" | "-s" | "--regexp-extended" | "--quiet" | "--silent" | "--separate" | "--in-place" => {
                    in_place |= text == "--in-place";
                }
                t if t.starts_with("--expression=") => scripts.push(&t["--expression=".len()..]),
                t if t.starts_with("--in-place=") || (t.starts_with("-i") && !t.starts_with("--")) => {
                    let suffix = t.strip_prefix("--in-place=").unwrap_or_else(|| &t[2..]);
                    if !backup_suffix(suffix) {
                        return Err(Keeps::Uncertain);
                    }
                    in_place = true;
                }
                _ => return Err(Keeps::NotFileChange),
            }
        }
        if !in_place {
            return Err(Keeps::NotFileChange);
        }
        if scripts.is_empty() {
            let script = positional.first().ok_or(Keeps::Uncertain)?;
            scripts.push(&script.text);
            positional.remove(0);
        }
        if !scripts.iter().all(|script| safe_sed_script(script)) {
            return Err(Keeps::NotFileChange);
        }
        if positional.is_empty() {
            return Err(Keeps::Uncertain);
        }
        for file in positional {
            let real = self.written(file, false)?;
            if !std::fs::metadata(&real).is_ok_and(|m| m.is_file()) {
                return Err(Keeps::Uncertain);
            }
        }
        Ok(())
    }

    fn truncate(&self, args: &[Word]) -> Result<(), Keeps> {
        let (mut size, mut files) = (false, vec![]);
        let mut words = args.iter();
        while let Some(word) = words.next() {
            match word.text.as_str() {
                "-s" => size = words.next().is_some_and(|n| !n.glob && !n.text.is_empty()),
                "-c" | "--no-create" => {}
                t if (t.starts_with("-s") && t.len() > 2) || (t.starts_with("--size=") && t.len() > 7) => size = true,
                t if t.starts_with('-') && !word.glob => return Err(Keeps::NotFileChange),
                _ => files.push(word),
            }
        }
        if !size || files.is_empty() {
            return Err(Keeps::NotFileChange);
        }
        files.into_iter().try_for_each(|file| self.written(file, true).map(drop))
    }

    /// `chmod <mode> files`: no option, so the first word is always the mode, `-x` included.
    fn chmod(&self, args: &[Word]) -> Result<(), Keeps> {
        let [mode, files @ ..] = args else { return Err(Keeps::NotFileChange) };
        let mode_chars = |c: char| matches!(c, '0'..='7' | 'u' | 'g' | 'o' | 'a' | 'r' | 'w' | 'x' | 'X' | 's' | 't' | '+' | '-' | '=' | ',');
        if mode.glob || mode.text.is_empty() || !mode.text.chars().all(mode_chars) || files.is_empty() {
            return Err(Keeps::NotFileChange);
        }
        files.iter().try_for_each(|file| self.written(file, true).map(drop))
    }

    /// `git checkout [--ours|--theirs] -- paths` and `git restore [options] [--] paths`, run in
    /// the worktree that holds the folder: the paths stay in that same worktree.
    fn git(&self, args: &[Word]) -> Result<(), Keeps> {
        let Some((sub, rest)) = args.split_first() else { return Err(Keeps::NotFileChange) };
        let checkout = match sub.text.as_str() {
            "checkout" => true,
            "restore" => false,
            _ => return Err(Keeps::NotFileChange),
        };
        let (mut paths, mut ended) = (vec![], false);
        let mut words = rest.iter();
        while let Some(word) = words.next() {
            let text = word.text.as_str();
            if ended {
                paths.push(word);
                continue;
            }
            if text == "--" {
                ended = true;
                continue;
            }
            if !text.starts_with('-') || word.glob {
                if checkout {
                    return Err(Keeps::NotFileChange);
                }
                paths.push(word);
                continue;
            }
            let known = match (checkout, text) {
                (_, "--ours" | "--theirs" | "-q" | "--quiet") => true,
                (false, "-W" | "--worktree" | "-S" | "--staged") => true,
                (false, "-s" | "--source") => words.next().is_some_and(|rev| !rev.glob && safe_ref(&rev.text)),
                (false, t) if t.starts_with("--source=") => safe_ref(&t["--source=".len()..]),
                _ => false,
            };
            if !known {
                return Err(Keeps::NotFileChange);
            }
        }
        if checkout && !ended {
            return Err(Keeps::NotFileChange);
        }
        if paths.is_empty() {
            return Err(Keeps::NotFileChange);
        }
        let repository = self.root_holding(&self.base).ok_or(Keeps::Outside)?;
        for path in paths {
            if path.glob || path.text.starts_with(':') || path.text.contains(glob_char) {
                return Err(Keeps::Uncertain);
            }
            let real = self.entry(path)?;
            self.owned(&real)?;
            if self.root_holding(&real) != Some(repository) {
                return Err(Keeps::Outside);
            }
        }
        Ok(())
    }
}

fn backup_suffix(suffix: &str) -> bool {
    !suffix.contains(['/', '*'])
}

/// A ref named by plain characters only, as `task_git` reads them.
fn safe_ref(text: &str) -> bool {
    !text.is_empty() && !text.starts_with(['-', '/']) && !text.contains("..") && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-' | '~' | '^'))
}

/// A sed script of `s` and `d` commands only, each with an optional line, `$` or `/regex/`
/// address (or a range of two), separated by `;` or new lines. No flag that writes a file (`w`)
/// or runs one (`e`), and no other command.
pub(crate) fn safe_sed_script(script: &str) -> bool {
    let chars: Vec<char> = script.chars().collect();
    let mut at = 0;
    let mut commands = 0;
    let skip_blank = |at: &mut usize| {
        while chars.get(*at).is_some_and(|c| matches!(c, ' ' | '\t' | ';' | '\n')) {
            *at += 1;
        }
    };
    // Up to the next unescaped `delimiter`, which it passes; false at the end or a new line.
    let delimited = |at: &mut usize, delimiter: char| -> bool {
        loop {
            match chars.get(*at) {
                None | Some('\n') => return false,
                Some('\\') => {
                    if chars.get(*at + 1).is_none_or(|c| *c == '\n') {
                        return false;
                    }
                    *at += 2;
                }
                Some(c) if *c == delimiter => {
                    *at += 1;
                    return true;
                }
                Some(_) => *at += 1,
            }
        }
    };
    let address = |at: &mut usize| -> bool {
        match chars.get(*at) {
            Some('$') => *at += 1,
            Some(c) if c.is_ascii_digit() => {
                while chars.get(*at).is_some_and(char::is_ascii_digit) {
                    *at += 1;
                }
            }
            Some('/') => {
                *at += 1;
                if !delimited(at, '/') {
                    return false;
                }
            }
            _ => {}
        }
        true
    };
    loop {
        skip_blank(&mut at);
        let Some(_) = chars.get(at) else { return commands > 0 };
        let start = at;
        if !address(&mut at) {
            return false;
        }
        if at > start && chars.get(at) == Some(&',') {
            at += 1;
            let second = at;
            if !address(&mut at) || at == second {
                return false;
            }
        }
        while chars.get(at).is_some_and(|c| matches!(c, ' ' | '\t')) {
            at += 1;
        }
        match chars.get(at) {
            Some('d') => at += 1,
            Some('s') => {
                at += 1;
                let Some(&delimiter) = chars.get(at) else { return false };
                if matches!(delimiter, '\\' | '\n' | ' ' | '\t' | ';') || delimiter.is_ascii_alphanumeric() {
                    return false;
                }
                at += 1;
                if !delimited(&mut at, delimiter) || !delimited(&mut at, delimiter) {
                    return false;
                }
                while chars.get(at).is_some_and(|c| matches!(c, 'g' | 'p' | 'I' | 'i' | 'M' | 'm' | '0'..='9')) {
                    at += 1;
                }
            }
            _ => return false,
        }
        while chars.get(at).is_some_and(|c| matches!(c, ' ' | '\t')) {
            at += 1;
        }
        if !matches!(chars.get(at), None | Some(';' | '\n')) {
            return false;
        }
        commands += 1;
    }
}

#[cfg(test)]
#[path = "own_worktree_tests.rs"]
mod tests;
