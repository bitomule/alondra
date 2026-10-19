//! «Allow always» on this Mac: which repo a node's prompt belongs to, the rules its card offers,
//! and the repo's `.claude/settings.local.json` in its main checkout, where Claude Code keeps
//! them. Claude Code writes a rule a held hook's card allowed always; for a decision asked through
//! the SDK Alondra adds it, and it takes out exactly the rule the user removes, leaving every
//! other byte as it was.

use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use alondra_core::allow_always::{self as rules, AllowAlways, Mode, Place, Refusal, Why};
use anyhow::{bail, Context, Result};
use serde_json::Value;

pub(crate) const SETTINGS: &str = ".claude/settings.local.json";

/// The repo a node works in: its main checkout, which keeps the local settings every worktree
/// shares, and the worktree the session runs in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Repo {
    pub root: PathBuf,
    pub worktree: PathBuf,
}
impl Repo {
    pub fn name(&self) -> String {
        self.root.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
    }
    pub fn settings(&self) -> PathBuf {
        self.root.join(SETTINGS)
    }
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned()).filter(|text| !text.is_empty())
}

/// The repo whose main checkout can keep this session's rules, as Claude Code decides it: none
/// outside git, when the checkout is the home folder, when the checkout, its `.git` or its
/// `.claude` is not the user's or `.claude` is a link, and none when the settings file is
/// tracked, since Claude Code then treats its rules as the repo's.
pub(crate) fn repo_of(cwd: &Path) -> Option<Repo> {
    let worktree = std::fs::canonicalize(git(cwd, &["rev-parse", "--show-toplevel"])?).ok()?;
    let common = std::fs::canonicalize(git(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?).ok()?;
    if common.file_name()? != ".git" {
        return None;
    }
    let root = common.parent()?.to_path_buf();
    let home = std::env::var_os("HOME").and_then(|home| std::fs::canonicalize(home).ok());
    if home.as_deref() == Some(root.as_path()) {
        return None;
    }
    let uid = unsafe { libc::getuid() };
    let owned = |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.uid() == uid);
    if !owned(&root) || !owned(&common) {
        return None;
    }
    match std::fs::symlink_metadata(root.join(".claude")) {
        Ok(meta) if meta.file_type().is_symlink() || meta.uid() != uid => return None,
        _ => {}
    }
    if git(&root, &["ls-files", "--error-unmatch", SETTINGS]).is_some() {
        return None;
    }
    Some(Repo { root, worktree })
}

/// What a node's permission card offers beyond allowing once: `None` for a prompt that is not a
/// command or comes from a hook that does not say where it runs.
pub(crate) fn assess(tool: Option<&str>, input: &Value, cwd: Option<&str>, mode: Option<&str>, suggestions: &Value, home: &Path) -> Option<AllowAlways> {
    if tool != Some(rules::TOOL) {
        return None;
    }
    let command = input.get("command")?.as_str()?;
    let cwd = std::fs::canonicalize(cwd?).ok()?;
    let Some(repo) = repo_of(&cwd) else {
        return Some(Refusal::new("", Why::Repo).into());
    };
    let files = home.join("files");
    let files = std::fs::canonicalize(&files).unwrap_or(files);
    let place = Place { cwd: &cwd, worktree: &repo.worktree, files: &files, mode: Mode::parse(mode) };
    Some(match rules::offer(command, &place, suggestions) {
        Err(refusal) => refusal.into(),
        Ok(offered) => {
            let present = allowed(&repo.settings()).unwrap_or_default();
            if offered.iter().all(|rule| present.contains(&rules::settings_entry(rule))) {
                log!("allow always: {} already allows {offered:?} and still asked for {command:?}", repo.settings().display());
                Refusal::new("", Why::AlreadyAllowed).into()
            } else {
                AllowAlways::Offered { repo: repo.name(), rules: offered, quiet: rules::quiet_parts(command) }
            }
        }
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuleState {
    /// Chosen; Claude Code has not been seen writing it yet.
    Pending,
    Active,
    /// Claude Code never wrote it: it is not listed, and its card says so.
    Missing,
    Removed,
}
impl RuleState {
    pub fn as_str(self) -> &'static str {
        match self {
            RuleState::Pending => "pending",
            RuleState::Active => "active",
            RuleState::Missing => "missing",
            RuleState::Removed => "removed",
        }
    }
    pub fn parse(text: &str) -> Self {
        match text {
            "active" => RuleState::Active,
            "missing" => RuleState::Missing,
            "removed" => RuleState::Removed,
            _ => RuleState::Pending,
        }
    }
}

/// A rule «allow always» added on this Mac.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AllowedRule {
    pub id: i64,
    /// The main checkout's folder.
    pub repo: String,
    pub settings: String,
    /// As Claude Code matches it: `make build *`.
    pub rule: String,
    pub added_at: i64,
    pub task_id: Option<String>,
    pub interaction_id: String,
    pub machine: String,
    pub state: RuleState,
}

fn machine() -> String {
    let mut name = [0u8; 256];
    let read = unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) } == 0;
    let end = name.iter().position(|b| *b == 0).unwrap_or(name.len());
    if read { String::from_utf8_lossy(&name[..end]).into_owned() } else { String::new() }
}

#[cfg(not(test))]
const WRITE_WAIT: std::time::Duration = std::time::Duration::from_secs(15);
#[cfg(test)]
const WRITE_WAIT: std::time::Duration = std::time::Duration::from_millis(300);

/// Each rule chosen for the repo `cwd` belongs to, remembered as pending; the repo's settings file,
/// or `None` outside a repo that can keep them.
fn remember(domain: &crate::domain_runtime::DomainRuntime, cwd: Option<&str>, offered: &[String], task_id: Option<&str>, interaction_id: &str) -> Option<PathBuf> {
    let repo = cwd.and_then(|cwd| std::fs::canonicalize(cwd).ok()).and_then(|cwd| repo_of(&cwd))?;
    let settings = repo.settings();
    let now = alondra_core::now_millis();
    let recorded: Vec<AllowedRule> = offered.iter().map(|rule| AllowedRule {
        id: 0, repo: repo.root.to_string_lossy().into_owned(), settings: settings.to_string_lossy().into_owned(), rule: rule.clone(),
        added_at: now, task_id: task_id.map(str::to_owned), interaction_id: interaction_id.to_owned(), machine: machine(), state: RuleState::Pending,
    }).collect();
    let store = domain.store.lock().expect("domain store poisoned");
    for rule in &recorded {
        if let Err(error) = store.record_allowed_rule(rule) {
            log!("allow always: could not remember {}: {error:#}", rule.rule);
        }
    }
    Some(settings)
}

/// The user chose «allow always»: each rule is remembered, and once Claude Code has had time to
/// write them, each is active if the repo's settings file has it, or missing if not.
pub(crate) async fn chosen(domain: &std::sync::Arc<crate::domain_runtime::DomainRuntime>, cwd: Option<&str>, rules: &[String], task_id: Option<&str>, interaction_id: &str) -> Result<()> {
    let recording = domain.clone();
    let cwd = cwd.map(str::to_owned);
    let offered = rules.to_vec();
    let task_id = task_id.map(str::to_owned);
    let interaction_id = interaction_id.to_owned();
    let settings = domain.hook_work.run(move || remember(&recording, cwd.as_deref(), &offered, task_id.as_deref(), &interaction_id)).await?;
    let Some(settings) = settings else { return Ok(()) };
    let started = std::time::Instant::now();
    while started.elapsed() < WRITE_WAIT {
        let settings = settings.clone();
        let offered = rules.to_vec();
        let written = domain.hook_work.run(move || {
            let present = allowed(&settings).unwrap_or_default();
            offered.iter().all(|rule| present.contains(&rules::settings_entry(rule)))
        }).await?;
        if written { break }
        wait_for_write(domain).await;
    }
    let recording = domain.clone();
    domain.hook_work.run(move || reconcile(&recording)).await?;
    Ok(())
}

/// The user chose «allow always» on a decision Claude asked for through the SDK, which never widens
/// Claude's permissions itself: Alondra writes each rule into the repo's settings file, where
/// Claude Code reads it for the next call, and remembers it as `chosen` does.
pub(crate) fn chosen_through_sdk(domain: &crate::domain_runtime::DomainRuntime, cwd: Option<&str>, rules: &[String], task_id: Option<&str>, interaction_id: &str) -> Result<()> {
    let Some(settings) = remember(domain, cwd, rules, task_id, interaction_id) else { return Ok(()) };
    add(&settings, &rules.iter().map(|rule| rules::settings_entry(rule)).collect::<Vec<_>>())?;
    reconcile(domain);
    Ok(())
}

#[cfg(test)]
pub(crate) struct ConfirmationWait {
    pub entered: tokio::sync::oneshot::Sender<()>,
    pub release: tokio::sync::oneshot::Receiver<()>,
}

async fn wait_for_write(_domain: &crate::domain_runtime::DomainRuntime) {
    #[cfg(test)]
    {
        let confirmation = _domain.allow_always_wait.lock().unwrap().take();
        if let Some(confirmation) = confirmation {
            confirmation.entered.send(()).unwrap();
            confirmation.release.await.unwrap();
            return;
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
}

/// The rules Alondra added, each checked against its settings file: a pending rule the file has
/// is active, one it still lacks after Claude Code's time to write it is missing, and an active
/// rule the file no longer has was removed by hand.
pub(crate) fn reconcile(domain: &crate::domain_runtime::DomainRuntime) -> Vec<AllowedRule> {
    let store = domain.store.lock().expect("domain store poisoned");
    let mut rows = store.allowed_rules().unwrap_or_default();
    let now = alondra_core::now_millis();
    let mut files: std::collections::BTreeMap<String, Option<Vec<String>>> = Default::default();
    for row in &mut rows {
        let present = files.entry(row.settings.clone()).or_insert_with(|| allowed(Path::new(&row.settings)).ok());
        let Some(present) = present else { continue };
        let has = present.contains(&rules::settings_entry(&row.rule));
        let next = match (row.state, has) {
            (RuleState::Pending | RuleState::Missing, true) => RuleState::Active,
            (RuleState::Pending, false) if now - row.added_at >= WRITE_WAIT.as_millis() as i64 => {
                log!("allow always: Claude Code did not write {} to {} for task {}", row.rule, row.settings, row.task_id.as_deref().unwrap_or("-"));
                RuleState::Missing
            }
            (RuleState::Active, false) => RuleState::Removed,
            (state, _) => state,
        };
        if next != row.state && store.set_allowed_rule_state(row.id, next).is_ok() {
            row.state = next;
        }
    }
    rows
}

/// Removes one rule Alondra added from its settings file. A rule the file no longer has counts as
/// removed.
pub(crate) fn remove_rule(domain: &crate::domain_runtime::DomainRuntime, id: i64) -> Result<Option<AllowedRule>> {
    let row = {
        let store = domain.store.lock().expect("domain store poisoned");
        store.allowed_rules()?.into_iter().find(|row| row.id == id)
    };
    let Some(row) = row else { return Ok(None) };
    remove(Path::new(&row.settings), &rules::settings_entry(&row.rule))?;
    domain.store.lock().expect("domain store poisoned").set_allowed_rule_state(row.id, RuleState::Removed)?;
    Ok(Some(row))
}

/// The `permissions.allow` entries of a settings file; none when it does not exist.
pub(crate) fn allowed(settings: &Path) -> Result<Vec<String>> {
    let text = match std::fs::read_to_string(settings) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error).with_context(|| format!("reading {}", settings.display())),
    };
    let value: Value = serde_json::from_str(&text).with_context(|| format!("{} is not valid JSON", settings.display()))?;
    Ok(entries_of(&value))
}

fn entries_of(value: &Value) -> Vec<String> {
    value.pointer("/permissions/allow").and_then(Value::as_array).into_iter().flatten()
        .filter_map(|entry| entry.as_str().map(str::to_owned)).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Removal {
    Removed,
    /// The rule was not in the file, or the file is gone: it is already removed.
    Absent,
}

/// Takes `entry` out of the file's `permissions.allow`, every time it appears, and nothing else:
/// unknown keys, order, spacing and the other rules stay byte for byte. The new file replaces
/// the old one in a single rename, so Claude Code never reads half of it. A missing file is not
/// created and a file that is not valid JSON is not touched.
pub(crate) fn remove(settings: &Path, entry: &str) -> Result<Removal> {
    let text = match std::fs::read_to_string(settings) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Removal::Absent),
        Err(error) => return Err(error).with_context(|| format!("reading {}", settings.display())),
    };
    let before: Value = serde_json::from_str(&text).with_context(|| format!("{} is not valid JSON", settings.display()))?;
    if !entries_of(&before).iter().any(|e| e == entry) {
        return Ok(Removal::Absent);
    }
    let mut expected = before.clone();
    if let Some(allow) = expected.pointer_mut("/permissions/allow").and_then(Value::as_array_mut) {
        allow.retain(|e| e.as_str() != Some(entry));
    }
    let edited = cut(&text, entry, &expected).with_context(|| format!("could not take the rule out of {}", settings.display()))?;
    let permissions = std::fs::metadata(settings)?.permissions().mode();
    let folder = settings.parent().context("settings file without a folder")?;
    let temp = folder.join(format!(".settings.local.json.alondra-{}", crate::auth::random_hex(8)));
    let written = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all(edited.as_bytes())?;
        file.set_permissions(std::fs::Permissions::from_mode(permissions & 0o7777))?;
        file.sync_all()?;
        std::fs::rename(&temp, settings)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written.with_context(|| format!("writing {}", settings.display()))?;
    Ok(Removal::Removed)
}

/// Adds each entry the file's `permissions.allow` lacks, creating the file and its folder when
/// there is none. The new file replaces the old one in a single rename, so Claude Code never reads
/// half of it; a file that is not valid JSON is not touched.
pub(crate) fn add(settings: &Path, entries: &[String]) -> Result<()> {
    let (mut value, permissions) = match std::fs::read_to_string(settings) {
        Ok(text) => (
            serde_json::from_str::<Value>(&text).with_context(|| format!("{} is not valid JSON", settings.display()))?,
            std::fs::metadata(settings)?.permissions().mode() & 0o7777,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (serde_json::json!({}), 0o600),
        Err(error) => return Err(error).with_context(|| format!("reading {}", settings.display())),
    };
    let present = entries_of(&value);
    let missing: Vec<&String> = entries.iter().filter(|entry| !present.contains(entry)).collect();
    if missing.is_empty() {
        return Ok(());
    }
    let object = value.as_object_mut().with_context(|| format!("{} is not a JSON object", settings.display()))?;
    let permissions_entry = object.entry("permissions").or_insert_with(|| serde_json::json!({}));
    let allow = permissions_entry
        .as_object_mut()
        .with_context(|| format!("{} has permissions that are not an object", settings.display()))?
        .entry("allow")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .with_context(|| format!("{} has a permissions.allow that is not a list", settings.display()))?;
    allow.extend(missing.into_iter().map(|entry| Value::String(entry.clone())));
    let folder = settings.parent().context("settings file without a folder")?;
    std::fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
    let temp = folder.join(format!(".settings.local.json.alondra-{}", crate::auth::random_hex(8)));
    let written = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all(serde_json::to_string_pretty(&value)?.as_bytes())?;
        file.write_all(b"\n")?;
        file.set_permissions(std::fs::Permissions::from_mode(permissions))?;
        file.sync_all()?;
        std::fs::rename(&temp, settings)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written.with_context(|| format!("writing {}", settings.display()))
}

/// The text without the string literal `entry`, together with the comma that joined it to its
/// neighbour, tried at each place the literal appears until the result reads as `expected`.
fn cut(text: &str, entry: &str, expected: &Value) -> Result<String> {
    let literal = serde_json::to_string(entry)?;
    let left = |value: &Value| entries_of(value).iter().filter(|e| *e == entry).count();
    let mut current = text.to_owned();
    let mut remaining = left(&serde_json::from_str(text)?);
    while remaining > 0 {
        let mut from = 0;
        let next = loop {
            let Some(found) = current[from..].find(&literal) else { bail!("the file changed in a way Alondra cannot follow") };
            let (start, end) = (from + found, from + found + literal.len());
            let candidate = without(&current, start, end);
            let fewer = serde_json::from_str::<Value>(&candidate).ok().filter(|value| left(value) + 1 == remaining && {
                let mut rest = value.clone();
                if let Some(allow) = rest.pointer_mut("/permissions/allow").and_then(Value::as_array_mut) {
                    allow.retain(|e| e.as_str() != Some(entry));
                }
                rest == *expected
            });
            if fewer.is_some() {
                break candidate;
            }
            from = end;
        };
        current = next;
        remaining -= 1;
    }
    Ok(current)
}

/// `text` without `[start, end)` and the comma on one side of it, with the spacing before it.
fn without(text: &str, start: usize, end: usize) -> String {
    let after = text[end..].char_indices().find(|(_, c)| !c.is_whitespace()).map(|(at, c)| (end + at, c));
    let before = text[..start].char_indices().rev().find(|(_, c)| !c.is_whitespace()).map(|(at, c)| (at, c));
    match (before, after) {
        (Some((b, _)), Some((a, ','))) => format!("{}{}", &text[..b + 1], &text[a + 1..]),
        (Some((b, ',')), _) => format!("{}{}", &text[..b], &text[end..]),
        (Some((b, _)), _) => format!("{}{}", &text[..b + 1], &text[end..]),
        (None, _) => format!("{}{}", &text[..start], &text[end..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_CODE: &str = "{\n  \"permissions\": {\n    \"allow\": [\n      \"Bash(git log *)\",\n      \"Bash(make build *)\",\n      \"Bash(cargo test *)\"\n    ],\n    \"deny\": [\"Bash(make build *)\"],\n    \"defaultMode\": \"default\"\n  },\n  \"zeta\": 1,\n  \"alpha\": {\"keep\": [1, 2]}\n}\n";

    fn scratch() -> crate::test_support::Directory {
        let dir = crate::test_support::Directory::new();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file(text: &str) -> (crate::test_support::Directory, PathBuf) {
        let dir = scratch();
        let path = dir.join("settings.local.json");
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    #[test]
    fn adding_rules_keeps_every_other_setting_and_never_repeats_one() {
        let (_dir, path) = file(CLAUDE_CODE);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        add(&path, &["Bash(make build *)".into(), "Bash(make test *)".into()]).unwrap();
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let mut expected: Value = serde_json::from_str(CLAUDE_CODE).unwrap();
        expected["permissions"]["allow"].as_array_mut().unwrap().push("Bash(make test *)".into());
        assert_eq!(after, expected);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777, 0o640);
        let written = std::fs::read(&path).unwrap();
        add(&path, &["Bash(make test *)".into()]).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), written, "a rule already there changes nothing");
    }

    #[test]
    fn adding_a_rule_where_there_are_no_settings_creates_them() {
        let dir = scratch();
        let path = dir.join(".claude/settings.local.json");
        add(&path, &["Bash(make build *)".into()]).unwrap();
        assert_eq!(allowed(&path).unwrap(), ["Bash(make build *)"]);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777, 0o600);
        std::fs::write(&path, "not json").unwrap();
        assert!(add(&path, &["Bash(make test *)".into()]).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json", "a file Alondra cannot read is not touched");
    }

    #[test]
    fn removing_a_rule_takes_out_exactly_that_rule() {
        let (_dir, path) = file(CLAUDE_CODE);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(remove(&path, "Bash(make build *)").unwrap(), Removal::Removed);
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, CLAUDE_CODE.replace("      \"Bash(make build *)\",\n", ""), "the deny rule, unknown keys and their order stay");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o640);
        assert_eq!(remove(&path, "Bash(make build *)").unwrap(), Removal::Absent, "removing twice is the same as once");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after);
    }

    #[test]
    fn the_first_last_and_only_rule_come_out_cleanly() {
        let (_dir, path) = file(CLAUDE_CODE);
        remove(&path, "Bash(cargo test *)").unwrap();
        remove(&path, "Bash(git log *)").unwrap();
        remove(&path, "Bash(make build *)").unwrap();
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["permissions"]["allow"], serde_json::json!([]));
        assert_eq!(after["permissions"]["deny"], serde_json::json!(["Bash(make build *)"]));
        let (_dir, path) = file(r#"{"permissions":{"allow":["Bash(a b *)","Bash(make build *)","Bash(a b *)"]}}"#);
        remove(&path, "Bash(a b *)").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"permissions":{"allow":["Bash(make build *)"]}}"#);
    }

    #[test]
    fn a_missing_or_broken_file_is_neither_created_nor_touched() {
        let dir = scratch();
        let missing = dir.join("settings.local.json");
        assert_eq!(remove(&missing, "Bash(make build *)").unwrap(), Removal::Absent);
        assert!(!missing.exists(), "removing never creates the file");
        let (_dir, broken) = file("{\"permissions\": {\"allow\": [\"Bash(make build *)\"");
        let error = remove(&broken, "Bash(make build *)").unwrap_err();
        assert!(error.to_string().contains("not valid JSON"), "{error:#}");
        assert_eq!(std::fs::read_to_string(&broken).unwrap(), "{\"permissions\": {\"allow\": [\"Bash(make build *)\"");
        assert_eq!(std::fs::read_dir(broken.parent().unwrap()).unwrap().count(), 1, "no temporary file is left behind");
    }

    #[test]
    fn a_literal_outside_the_allow_list_is_never_cut() {
        let text = r#"{"note":"Bash(make build *)","permissions":{"allow":["Bash(make build *)"]}}"#;
        let (_dir, path) = file(text);
        remove(&path, "Bash(make build *)").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"note":"Bash(make build *)","permissions":{"allow":[]}}"#);
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let status = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(status.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&status.stderr));
    }

    fn repo() -> (crate::test_support::Directory, PathBuf) {
        let dir = scratch();
        let root = dir.join("app");
        std::fs::create_dir_all(&root).unwrap();
        git_in(&root, &["init", "-q", "-b", "main"]);
        git_in(&root, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "first"]);
        (dir, root)
    }

    #[test]
    fn a_worktree_keeps_its_rules_in_the_main_checkout() {
        let (_dir, root) = repo();
        let worktree = root.join(".claude/worktrees/t-1");
        git_in(&root, &["worktree", "add", "-q", "-b", "t-1", worktree.to_str().unwrap()]);
        let found = repo_of(&worktree.join(".")).unwrap();
        assert_eq!(found, Repo { root: root.clone(), worktree: worktree.clone() });
        assert_eq!(found.settings(), root.join(".claude/settings.local.json"));
        assert_eq!(found.name(), "app");
    }

    #[test]
    fn a_repo_that_cannot_keep_local_rules_offers_none() {
        let outside = scratch();
        assert_eq!(repo_of(&outside), None, "no git");
        let (_dir, root) = repo();
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::write(root.join(SETTINGS), "{}").unwrap();
        git_in(&root, &["add", "-f", SETTINGS]);
        assert_eq!(repo_of(&root), None, "a tracked settings file is the repo's, not this Mac's");
        let (_dir, root) = repo();
        let elsewhere = scratch();
        std::os::unix::fs::symlink(&*elsewhere, root.join(".claude")).unwrap();
        assert_eq!(repo_of(&root), None, "a linked .claude");
    }

    #[test]
    fn the_card_offers_the_rules_unless_the_file_already_has_them() {
        let (_dir, root) = repo();
        let home = scratch();
        let input = serde_json::json!({"command": "make build 2>&1 | tail -1"});
        let cwd = root.to_str().unwrap();
        let offered = assess(Some("Bash"), &input, Some(cwd), Some("default"), &Value::Null, &*home);
        assert_eq!(offered, Some(AllowAlways::Offered { repo: "app".into(), rules: vec!["make build *".into()], quiet: vec!["tail".into()] }));
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::write(root.join(SETTINGS), r#"{"permissions":{"allow":["Bash(make build *)"]}}"#).unwrap();
        let offered = assess(Some("Bash"), &input, Some(cwd), Some("default"), &Value::Null, &*home);
        assert_eq!(offered, Some(Refusal::new("", Why::AlreadyAllowed).into()), "it would not stop asking");
        assert_eq!(assess(Some("Edit"), &input, Some(cwd), None, &Value::Null, &*home), None);
        assert_eq!(assess(Some("Bash"), &input, None, None, &Value::Null, &*home), None, "an old hook says nothing about it");
        let destructive = serde_json::json!({"command": "make build && rm -rf out"});
        assert_eq!(assess(Some("Bash"), &destructive, Some(cwd), Some("default"), &Value::Null, &*home),
            Some(Refusal::new("rm -rf out", Why::Deletes).into()));
    }
}
