use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitCode, Stdio};

use alondra_ablation::items::remove_at_lines;
use alondra_ablation::plan::{Plan, Tree, plan};
use alondra_ablation::report::{Outcome, Row, compile_errors, judge, parse_libtest, render};
use alondra_ablation::scratch::{Lock, Scratch, WRITING, restore};
use anyhow::{Context, Result, bail};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

const USAGE: &str = "usage: ablation [--base <branch>] [--restore]

Runs the Rust tests the working tree adds or edits against the base branch's production
code, and requires them to fail there and pass as they are. Uncommitted changes count.
The base is the merge-base with origin/<branch>, origin/main by default.";

const MAX_COMPILE_ROUNDS: usize = 8;
/// Unoptimized builds: each run recompiles the changed crates up to a few times, and the
/// result of a test does not depend on optimization.
const FAST: &str = "--compilation_mode=fastbuild";
/// What the verdict's fingerprint covers; `.musts/extensions/ablation/ablation-check.sh`
/// lists the same paths.
const FINGERPRINT_PATHS: &[&str] = &[
    "crates",
    "fixtures",
    "Cargo.toml",
    "Cargo.lock",
    "MODULE.bazel",
    ".bazelrc",
    ".bazelversion",
    ":(glob)**/BUILD.bazel",
    ":(glob)**/BUILD",
    ":(glob)**/*.bzl",
];

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(err) => {
            println!("ABLATION FAIL: {err:#}");
            ExitCode::from(2)
        }
    }
}

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn git(&self, args: &[&str]) -> Result<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()
            .context("git")?;
        if !out.status.success() {
            bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8(out.stdout)?)
    }

    fn lines(&self, args: &[&str]) -> Result<Vec<String>> {
        Ok(self.git(args)?.lines().map(String::from).collect())
    }

    fn git_path(&self, name: &str) -> Result<PathBuf> {
        let path = self.git(&["rev-parse", "--git-path", name])?;
        Ok(self.root.join(path.trim()))
    }

    fn with_paths<'a>(&self, args: &[&'a str]) -> Vec<&'a str> {
        let mut all = args.to_vec();
        all.push("--");
        all.extend_from_slice(FINGERPRINT_PATHS);
        all
    }
}

struct CatFile {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl CatFile {
    fn new(root: &Path) -> Result<Self> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("git cat-file")?;
        let stdin = child.stdin.take().context("stdin")?;
        let stdout = BufReader::new(child.stdout.take().context("stdout")?);
        Ok(Self { _child: child, stdin, stdout })
    }

    fn read(&mut self, rev: &str, path: &str) -> Option<String> {
        writeln!(self.stdin, "{rev}:{path}").ok()?;
        self.stdin.flush().ok()?;
        let mut header = String::new();
        self.stdout.read_line(&mut header).ok()?;
        let size: usize = header.trim_end().rsplit(' ').next()?.parse().ok()?;
        let mut body = vec![0; size + 1];
        self.stdout.read_exact(&mut body).ok()?;
        body.pop();
        String::from_utf8(body).ok()
    }
}

fn bazel() -> String {
    std::env::var("BAZEL").unwrap_or_else(|_| "bazelisk".into())
}

fn bazel_out(root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new(bazel())
        .current_dir(root)
        .args(args)
        .stderr(Stdio::inherit())
        .output()
        .context("bazel")?;
    if !out.status.success() {
        bail!("bazel {} failed", args.join(" "));
    }
    Ok(String::from_utf8(out.stdout)?)
}

fn bazel_status(root: &Path, args: &[String]) -> Result<bool> {
    Ok(Command::new(bazel())
        .current_dir(root)
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("bazel")?
        .success())
}

/// Fingerprint of the changed files under `FINGERPRINT_PATHS` as they are on disk now. The
/// musts extension recomputes it, so a verdict about other code is refused.
fn fingerprint(repo: &Repo, base: &str) -> Result<String> {
    let mut paths: BTreeSet<String> = repo
        .lines(&repo.with_paths(&["diff", "--name-only", "--no-renames", base]))?
        .into_iter()
        .collect();
    paths.extend(repo.lines(&repo.with_paths(&["ls-files", "--others", "--exclude-standard"]))?);
    let mut listing = String::new();
    for path in &paths {
        let hash = if repo.root.join(path).is_file() {
            repo.git(&["hash-object", "--", path])?.trim().to_string()
        } else {
            "deleted".to_string()
        };
        listing.push_str(&format!("{path} {hash}\n"));
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(&repo.root)
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child.stdin.take().context("stdin")?.write_all(listing.as_bytes())?;
    let out = child.wait_with_output()?;
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}

/// On SIGINT, SIGTERM or SIGHUP, puts the branch back before exiting.
fn restore_on_signal(root: PathBuf, guard: PathBuf, lock: PathBuf) -> Result<()> {
    let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
    std::thread::spawn(move || {
        if let Some(signal) = signals.forever().next() {
            let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
            let code = match restore(&root, &guard) {
                Ok(()) => {
                    eprintln!("Interrupted: the branch's files are back.");
                    128 + signal
                }
                Err(err) => {
                    eprintln!("Interrupted: {err:#}");
                    3
                }
            };
            let _ = std::fs::remove_file(&lock);
            std::process::exit(code);
        }
    });
    Ok(())
}

fn label_log(testlogs: &Path, label: &str) -> PathBuf {
    let rest = label.trim_start_matches("//");
    let (pkg, name) = rest.split_once(':').unwrap_or((rest, ""));
    testlogs.join(pkg).join(name).join("test.log")
}

fn label_path(label: &str) -> String {
    let rest = label.trim_start_matches("//");
    match rest.split_once(':') {
        Some(("", file)) => file.to_string(),
        Some((pkg, file)) => format!("{pkg}/{file}"),
        None => rest.to_string(),
    }
}

#[derive(Default)]
struct TestRun {
    outcomes: HashMap<(String, String), Outcome>,
    labels_without_results: BTreeSet<String>,
    build_log: String,
}

fn run_tests(root: &Path, testlogs: &Path, labels: &[String], names: &[String]) -> Result<TestRun> {
    let mut run = TestRun::default();
    if labels.is_empty() {
        return Ok(run);
    }
    for label in labels {
        let _ = std::fs::remove_file(label_log(testlogs, label));
    }
    let mut args: Vec<String> = [
        "test",
        FAST,
        "--keep_going",
        "--nocache_test_results",
        "--test_output=summary",
        "--test_arg=--exact",
    ]
    .map(String::from)
    .to_vec();
    args.extend(names.iter().map(|n| format!("--test_arg={n}")));
    args.extend(labels.iter().cloned());
    let mut child = Command::new(bazel())
        .current_dir(root)
        .args(&args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .spawn()
        .context("bazel")?;
    for line in BufReader::new(child.stderr.take().context("stderr")?).lines() {
        let line = line?;
        eprintln!("{line}");
        run.build_log.push_str(&line);
        run.build_log.push('\n');
    }
    child.wait()?;

    for label in labels {
        match std::fs::read_to_string(label_log(testlogs, label)) {
            Ok(log) => run.outcomes.extend(
                parse_libtest(&log).into_iter().map(|(name, o)| ((label.clone(), name), o)),
            ),
            Err(_) => {
                run.labels_without_results.insert(label.clone());
            }
        }
    }
    Ok(run)
}

/// `origin/<branch>` when that remote branch exists, else the ref as given.
fn resolve_base_ref(repo: &Repo, given: Option<String>) -> String {
    let given = given.unwrap_or_else(|| "main".into());
    let remote = if given.starts_with("origin/") { given.clone() } else { format!("origin/{given}") };
    if repo.git(&["rev-parse", "--verify", "--quiet", &format!("{remote}^{{commit}}")]).is_ok() {
        remote
    } else {
        given
    }
}

fn run() -> Result<bool> {
    let mut base_ref = None;
    let mut only_restore = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--base" => base_ref = Some(args.next().context("--base needs a branch")?),
            "--restore" => only_restore = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(true);
            }
            other => bail!("unknown argument {other}\n{USAGE}"),
        }
    }
    let start = match std::env::var("BUILD_WORKSPACE_DIRECTORY") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => std::env::current_dir()?,
    };
    let top = Repo { root: start }.git(&["rev-parse", "--show-toplevel"])?;
    let repo = Repo { root: PathBuf::from(top.trim()) };
    let guard = repo.git_path("alondra-ablation")?;
    let lock = Lock::acquire(&repo.git_path("alondra-ablation.lock")?)?;
    restore_on_signal(repo.root.clone(), guard.clone(), lock.path().to_path_buf())?;

    if guard.join("manifest").exists() {
        restore(&repo.root, &guard)?;
        println!("Restored the files an interrupted ablation run had replaced.");
    }
    if only_restore {
        return Ok(true);
    }

    let base_ref = resolve_base_ref(&repo, base_ref);
    let branch = repo.git(&["rev-parse", "--abbrev-ref", "HEAD"])?.trim().to_string();
    if base_ref == format!("origin/{branch}") {
        bail!("{base_ref} is this branch itself; pass the branch this one is merged into");
    }
    let base = repo.git(&["merge-base", &base_ref, "HEAD"])?.trim().to_string();
    let mut changed: BTreeSet<String> =
        repo.lines(&["diff", "--name-only", "--no-renames", &base])?.into_iter().collect();
    changed.extend(repo.lines(&repo.with_paths(&["ls-files", "--others", "--exclude-standard"]))?);
    let changed: Vec<String> = changed.into_iter().collect();
    let tree = fingerprint(&repo, &base)?;
    let verdict = format!("base={base} ref={base_ref} tree={tree}");
    println!("Base {base_ref} at {}, {} changed files.", &base[..12], changed.len());

    let plan: Plan = {
        let cat = RefCell::new(CatFile::new(&repo.root)?);
        let base_files = repo.lines(&["ls-tree", "-r", "--name-only", &base, "--", "crates"])?;
        let head_files: Vec<String> = repo
            .lines(&["ls-files", "--cached", "--others", "--exclude-standard", "--", "crates"])?
            .into_iter()
            .filter(|p| repo.root.join(p).is_file())
            .collect();
        let read_base = |p: &str| cat.borrow_mut().read(&base, p);
        let read_head = |p: &str| std::fs::read_to_string(repo.root.join(p)).ok();
        plan(
            &changed,
            Tree { files: &base_files, read: &read_base },
            Tree { files: &head_files, read: &read_head },
        )?
    };

    if plan.targets.is_empty() {
        print!("{}", render(&[], &plan.uncovered));
        println!("No new or changed Rust tests.");
        println!("ABLATION PASS {verdict}");
        return Ok(true);
    }

    let names: Vec<String> = plan.targets.iter().map(|t| t.name.clone()).collect();
    let crates: BTreeSet<&str> = plan.targets.iter().map(|t| t.crate_dir.as_str()).collect();
    let base_crates: BTreeSet<&str> = plan
        .targets
        .iter()
        .filter(|t| t.exists_without_change)
        .map(|t| t.crate_dir.as_str())
        .collect();
    let query = |expr: &str| -> Result<Vec<String>> {
        Ok(bazel_out(&repo.root, &["query", expr])?.lines().map(String::from).collect())
    };
    let union = |crates: &BTreeSet<&str>| {
        crates.iter().map(|c| format!("//{c}/...")).collect::<Vec<_>>().join(" + ")
    };
    let testlogs = PathBuf::from(bazel_out(&repo.root, &["info", FAST, "bazel-testlogs"])?.trim());
    let with_labels = query(&format!("tests({})", union(&crates)))?;
    let mut labels_of_file: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for label in &with_labels {
        for src in query(&format!("labels(srcs, {label}) + labels(srcs, labels(crate, {label}))"))? {
            labels_of_file.entry(label_path(&src)).or_default().push(label.clone());
        }
    }
    let without_labels: Vec<String> = with_labels
        .iter()
        .filter(|l| base_crates.iter().any(|c| l.starts_with(&format!("//{c}:"))))
        .cloned()
        .collect();
    let code_labels = if base_crates.is_empty() {
        vec![]
    } else {
        query(&format!("kind(\"rust_library|rust_binary\", {})", union(&base_crates)))?
    };

    println!("\n== Without the change: {} tests on the base's production code ==", names.len());
    let mut removed: BTreeSet<String> = BTreeSet::new();
    let without = (|| -> Result<Option<TestRun>> {
        let mut scratch = Scratch::new(&repo.root, &guard)?;
        for (path, content) in &plan.writes {
            scratch.write(path, content.as_deref())?;
        }
        if !code_labels.is_empty() {
            let mut args = vec!["build".to_string(), FAST.to_string(), "--keep_going".to_string()];
            args.extend(code_labels.iter().cloned());
            if !bazel_status(&repo.root, &args)? {
                return Ok(None);
            }
        }
        for _ in 0..MAX_COMPILE_ROUNDS {
            let run = run_tests(&repo.root, &testlogs, &without_labels, &names)?;
            if run.labels_without_results.is_empty() {
                return Ok(Some(run));
            }
            let mut by_file: BTreeMap<String, Vec<usize>> = BTreeMap::new();
            for (path, line) in compile_errors(&run.build_log) {
                if base_crates.iter().any(|c| path.starts_with(&format!("{c}/"))) {
                    by_file.entry(path).or_default().push(line);
                }
            }
            let mut progressed = false;
            for (path, lines) in by_file {
                let Ok(text) = std::fs::read_to_string(repo.root.join(&path)) else { continue };
                let removal = remove_at_lines(&text, &lines, plan.test_files.contains(&path))?;
                if removal.text == text {
                    continue;
                }
                progressed = true;
                scratch.write(&path, Some(&removal.text))?;
                for local in removal.removed_tests {
                    removed.extend(
                        plan.targets
                            .iter()
                            .filter(|t| {
                                t.file == path
                                    && (t.name == local || t.name.ends_with(&format!("::{local}")))
                            })
                            .map(|t| t.name.clone()),
                    );
                }
            }
            if !progressed {
                return Ok(Some(run));
            }
            println!("\n== Took out the test code that does not compile without the change; running again ==");
        }
        Ok(Some(run_tests(&repo.root, &testlogs, &without_labels, &names)?))
    })();
    restore(&repo.root, &guard)?;
    if fingerprint(&repo, &base)? != tree {
        bail!("the branch's files were not restored as they were; check `git status`");
    }
    let Some(without) = without? else {
        println!(
            "ABLATION FAIL: inconclusive, the base's production code does not build with this branch's tests and build files"
        );
        return Ok(false);
    };

    println!("\n== With the change ==");
    let with = run_tests(&repo.root, &testlogs, &with_labels, &names)?;

    let mut rows: Vec<Row> = Vec::new();
    for target in &plan.targets {
        let labels = labels_of_file.get(&target.file).cloned().unwrap_or_else(|| vec!["(no test target)".into()]);
        for label in labels {
            let key = (label.clone(), target.name.clone());
            rows.push(judge(
                target,
                &label,
                without.outcomes.get(&key).copied(),
                removed.contains(&target.name),
                without.labels_without_results.contains(&label),
                with.outcomes.get(&key).copied(),
            ));
        }
    }
    println!();
    print!("{}", render(&rows, &plan.uncovered));
    drop(lock);
    let failing = rows.iter().filter(|r| !r.ok()).count();
    if failing == 0 {
        println!("ABLATION PASS {verdict}");
        Ok(true)
    } else {
        println!("ABLATION FAIL: {failing} of {} tests are not proven", rows.len());
        Ok(false)
    }
}
