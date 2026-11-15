//! `bazel run //crates/scenarios:accept -- <case>`: runs the case and writes what its checkpoints
//! read into the case's folder in the source tree. The pull request shows the diff of those files.

fn main() {
    // SAFETY: nothing else runs yet; the views show local time and a case is accepted in UTC.
    unsafe { std::env::set_var("TZ", "UTC") };
    let Some(name) = std::env::args().nth(1) else {
        eprintln!("usage: accept <case>");
        std::process::exit(2);
    };
    let workspace = std::env::var_os("BUILD_WORKSPACE_DIRECTORY").expect("run it with `bazel run`");
    let case = std::path::PathBuf::from(workspace).join("crates/scenarios/cases").join(&name);
    match alondra_scenarios::accept(&case) {
        Ok(outcome) => {
            for (checkpoint, files) in outcome.checkpoints {
                println!("{checkpoint}: {} file(s) written", files.len());
            }
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
