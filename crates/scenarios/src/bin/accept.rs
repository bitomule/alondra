//! `bazel run //crates/scenarios:accept -- <case>...`: runs each case and writes what its
//! checkpoints read into the case's folder in the source tree. The pull request shows the diff of
//! those files.

fn main() {
    // SAFETY: nothing else runs yet; the views show local time and a case is accepted in UTC.
    unsafe { std::env::set_var("TZ", "UTC") };
    let until_failure = std::env::args().any(|argument| argument == "--until-failure");
    let names: Vec<String> = std::env::args().skip(1).filter(|argument| !argument.starts_with("--")).collect();
    if names.is_empty() {
        eprintln!("usage: accept <case>... [--until-failure]");
        std::process::exit(2);
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
    if failed {
        std::process::exit(1);
    }
}
