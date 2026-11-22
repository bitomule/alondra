//! `bazel run //crates/scenarios:accept -- <case>...`: runs each case and writes what its
//! checkpoints read into the case's folder in the source tree. The pull request shows the diff of
//! those files.

/// A short private folder for host sockets, which the test wrapper makes for `bazel test` but
/// `bazel run` does not. macOS caps a unix socket path at 104 bytes and its default temporary
/// folder is longer than the runner's limit.
struct ShortFolder(Option<std::path::PathBuf>);

impl ShortFolder {
    fn make() -> Self {
        if std::env::var_os("ALONDRA_SHORT_TMPDIR").is_some() {
            return Self(None);
        }
        let path = std::path::PathBuf::from(format!("/tmp/ac.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).expect("a short temporary folder under /tmp");
        // SAFETY: nothing else runs yet.
        unsafe { std::env::set_var("ALONDRA_SHORT_TMPDIR", &path) };
        Self(Some(path))
    }
}

impl Drop for ShortFolder {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn main() {
    let code = run();
    if code != 0 {
        std::process::exit(code);
    }
}

fn run() -> i32 {
    let _folder = ShortFolder::make();
    // SAFETY: nothing else runs yet; the views show local time and a case is accepted in UTC.
    unsafe { std::env::set_var("TZ", "UTC") };
    let until_failure = std::env::args().any(|argument| argument == "--until-failure");
    let names: Vec<String> = std::env::args().skip(1).filter(|argument| !argument.starts_with("--")).collect();
    if names.is_empty() {
        eprintln!("usage: accept <case>... [--until-failure]");
        return 2;
    }
    let workspace = std::env::var_os("BUILD_WORKSPACE_DIRECTORY").expect("run it with `bazel run`");
    let mut failed = false;
    for name in names {
        let case = std::path::PathBuf::from(&workspace).join("crates/scenarios/cases").join(&name);
        if until_failure {
            match alondra_scenarios::accept_until_failure(&case) {
                Ok(failure) => {
                    println!("{name}: checkpoints before the failure written");
                    if let Some(failure) = failure {
                        println!("{name}: {failure}");
                    }
                }
                Err(error) => {
                    eprintln!("{name}: {error}");
                    failed = true;
                }
            }
            continue;
        }
        match alondra_scenarios::accept(&case) {
            Ok(outcome) => {
                for (checkpoint, files) in outcome.checkpoints {
                    println!("{name}: {checkpoint}: {} file(s) written", files.len());
                }
            }
            Err(error) => {
                eprintln!("{name}: {error}");
                failed = true;
            }
        }
    }
    i32::from(failed)
}
