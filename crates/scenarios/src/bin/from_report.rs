//! `from_report <package> --name <case> --out <folder under .scratch>`: drafts a scenario from the
//! folder `alondra report` wrote on this Mac. The draft goes under a `.scratch` folder, never into
//! `cases/`, and nothing is written when the package or the draft carries a private value.

use alondra_scenarios::from_report::convert;
use alondra_scenarios_transcript::guard;
use std::path::PathBuf;

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let option = |name: &str| arguments.iter().position(|argument| argument == name).and_then(|at| arguments.get(at + 1));
    let (Some(package), Some(name), Some(out)) = (arguments.first().filter(|first| !first.starts_with("--")), option("--name"), option("--out")) else {
        fail("usage: from_report <package folder> --name <case> --out <folder under .scratch>")
    };
    let out = PathBuf::from(out);
    if !out.components().any(|part| part.as_os_str() == ".scratch") {
        fail("the draft is written under a .scratch folder; moving it into cases/ is for whoever completes it");
    }
    let draft = convert(std::path::Path::new(package), name, &out, &guard::literals_from_env()).unwrap_or_else(|refusal| fail(&refusal.to_string()));
    println!("{} written with {} gap(s) to complete; read gaps.txt first", out.display(), draft.gaps.lines().count().saturating_sub(1));
}
