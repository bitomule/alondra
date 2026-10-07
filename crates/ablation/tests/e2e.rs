//! The ablation binary against a throwaway git repository, with a fake `bazel` whose
//! answers each scenario sets.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BASE_LIB: &str = "pub fn limit() -> usize { 256 }\n";
const BRANCH_LIB: &str = "pub fn limit() -> usize { 128 }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn caught() {\n        assert_eq!(super::limit(), 128);\n    }\n}\n";
const LIB: &str = "crates/x/src/lib.rs";
const FIXED: &str = "limit() -> usize { 128 }";

fn runfile(var: &str) -> PathBuf {
    std::env::current_dir().unwrap().join(std::env::var(var).unwrap())
}

struct Fixture {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    fake: PathBuf,
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

impl Fixture {
    /// `main` holds the base library, `feature` the fix and its test; origin/main is main.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let fake = dir.path().join("fake");
        std::fs::create_dir_all(repo.join("crates/x/src")).unwrap();
        std::fs::create_dir_all(fake.join("srcs")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@example.com"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join(LIB), BASE_LIB).unwrap();
        std::fs::write(repo.join("Cargo.lock"), "base lock\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&repo, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(repo.join(LIB), BRANCH_LIB).unwrap();
        git(&repo, &["commit", "-q", "-am", "fix"]);

        let fixture = Self { _dir: dir, repo, fake };
        fixture.set("tests", "//crates/x:x_test\n");
        fixture.set("code", "//crates/x:x\n");
        fixture.set("srcs/__crates_x_x_test", "//crates/x:src/lib.rs\n");
        fixture.hook(&[("crates/x", "x_test", "tests::caught", "ok", "FAILED")]);
        fixture
    }

    fn set(&self, name: &str, text: &str) {
        std::fs::write(self.fake.join(name), text).unwrap();
    }

    /// Each entry writes `test <name> ... <with>` while lib.rs holds the fix, `<without>` otherwise.
    fn hook(&self, logs: &[(&str, &str, &str, &str, &str)]) {
        let mut script = String::new();
        for (pkg, target, name, with, without) in logs {
            script.push_str(&format!(
                "d=\"$FAKE_DIR/testlogs/{pkg}/{target}\"; mkdir -p \"$d\"\n\
                 if grep -qF '{FIXED}' {LIB}; then r='{with}'; else r='{without}'; fi\n\
                 echo \"test {name} ... $r\" > \"$d/test.log\"\n"
            ));
        }
        self.set("test-hook", &script);
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(runfile("ABLATION_BIN"));
        cmd.args(args)
            .current_dir(&self.repo)
            .env("BAZEL", runfile("FAKE_BAZEL"))
            .env("FAKE_DIR", &self.fake)
            .env_remove("BUILD_WORKSPACE_DIRECTORY")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn spawn_blocked(&self) -> Child {
        self.set("block", "");
        let child = self.command(&[]).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while !self.fake.join("building").exists() {
            assert!(Instant::now() < deadline, "the run never reached the build");
            std::thread::sleep(Duration::from_millis(20));
        }
        child
    }

    fn lib(&self) -> String {
        std::fs::read_to_string(self.repo.join(LIB)).unwrap()
    }

    fn guard(&self) -> PathBuf {
        self.repo.join(".git/alondra-ablation")
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn signal(child: &Child, signal: i32) {
    assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
}

#[test]
fn a_caught_test_passes_the_check() {
    let fixture = Fixture::new();
    let out = fixture.run(&[]);
    assert!(stdout(&out).contains("ABLATION PASS"), "{}", stdout(&out));
    assert_eq!(fixture.lib(), BRANCH_LIB);
}

#[test]
fn sigterm_during_the_run_puts_the_branch_back() {
    let fixture = Fixture::new();
    let child = fixture.spawn_blocked();
    assert_eq!(fixture.lib().contains("{ 256 }"), true, "the run should hold the base's code");
    signal(&child, libc::SIGTERM);
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(128 + libc::SIGTERM));
    assert_eq!(fixture.lib(), BRANCH_LIB);
    assert!(!fixture.guard().exists());
    assert!(!fixture.repo.join(".git/alondra-ablation.lock").exists());
}

#[test]
fn after_a_kill_the_backup_restores_without_any_build() {
    let fixture = Fixture::new();
    let mut child = fixture.spawn_blocked();
    signal(&child, libc::SIGKILL);
    child.wait().unwrap();
    assert!(fixture.lib().contains("{ 256 }"));

    let status = Command::new("bash").arg(fixture.guard().join("restore.sh")).current_dir(&fixture.repo).status().unwrap();
    assert!(status.success());
    assert_eq!(fixture.lib(), BRANCH_LIB);
    assert!(!fixture.guard().exists());
}

#[test]
fn a_file_edited_after_a_kill_is_never_overwritten() {
    let fixture = Fixture::new();
    let mut child = fixture.spawn_blocked();
    signal(&child, libc::SIGKILL);
    child.wait().unwrap();
    std::fs::write(fixture.repo.join(LIB), "edited after the interruption\n").unwrap();

    let out = fixture.run(&["--restore"]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert_eq!(fixture.lib(), "edited after the interruption\n");
    let saved = std::fs::read_to_string(fixture.guard().join("files").join(LIB)).unwrap();
    assert_eq!(saved, BRANCH_LIB);
}

#[test]
fn a_second_run_is_refused_and_leaves_the_first_alone() {
    let fixture = Fixture::new();
    let first = fixture.spawn_blocked();
    let second = fixture.run(&[]);
    assert!(!second.status.success());
    assert!(stdout(&second).contains("another ablation run"), "{}", stdout(&second));
    assert!(fixture.lib().contains("{ 256 }"), "the first run's tree was touched");
    assert!(fixture.guard().join("manifest").exists());

    signal(&first, libc::SIGTERM);
    first.wait_with_output().unwrap();
    assert_eq!(fixture.lib(), BRANCH_LIB);
}

#[test]
fn an_ignored_test_is_not_proven() {
    let fixture = Fixture::new();
    fixture.hook(&[("crates/x", "x_test", "tests::caught", "ignored", "ignored")]);
    let out = fixture.run(&[]);
    let text = stdout(&out);
    assert!(text.contains("IGNORED (#[ignore]): not proven"), "{text}");
    assert!(text.contains("ABLATION FAIL: 1 of 1 tests are not proven"), "{text}");
}

#[test]
fn outcomes_are_read_from_the_target_that_compiles_the_test() {
    let fixture = Fixture::new();
    fixture.set("tests", "//crates/x:x_test\n//crates/x:other_test\n");
    fixture.set("srcs/__crates_x_other_test", "//crates/x:tests/other.rs\n");
    fixture.hook(&[
        ("crates/x", "x_test", "tests::caught", "ok", "ok"),
        ("crates/x", "other_test", "tests::caught", "ok", "FAILED"),
    ]);
    let out = fixture.run(&[]);
    let text = stdout(&out);
    assert!(text.contains("PASSES: the test does not catch the change"), "{text}");
    assert!(text.contains("ABLATION FAIL: 1 of 1 tests are not proven"), "{text}");
}
