//! Writes the crash report's log catalog and vocabulary in two kinds of Bazel action, so the CLI
//! recompiles only when a crate's part of them changes:
//!
//! - `fragment CATALOG WORDS [--log] [--words] FILES...`: one crate's part, from its production
//!   sources. A test file among them fails the action: the catalog never depends on a test.
//! - `join CATALOG VOCABULARY FRAGMENTS...`: the whole catalog and vocabulary from the crates'
//!   `.catalog` and `.words` parts, the catalogs in the order given.
#[path = "src/report/extract.rs"]
#[allow(dead_code)]
mod extract;

use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().expect("fragment or join");
    let catalog_path = args.next().expect("the catalog path");
    let words_path = args.next().expect("the vocabulary path");
    let rest: Vec<String> = args.collect();
    let (catalog, words) = match mode.as_str() {
        "fragment" => fragment(&rest),
        "join" => join(&rest),
        other => panic!("unknown mode {other}: fragment or join"),
    };
    std::fs::write(catalog_path, catalog).expect("write the catalog");
    std::fs::write(words_path, words).expect("write the vocabulary");
}

fn fragment(args: &[String]) -> (String, String) {
    let log = args.iter().any(|arg| arg == "--log");
    let words = args.iter().any(|arg| arg == "--words");
    let mut files: Vec<(PathBuf, String)> = args
        .iter()
        .filter(|arg| !arg.starts_with("--"))
        .map(|file| (PathBuf::from(file), std::fs::read_to_string(file).unwrap_or_else(|error| panic!("read {file}: {error}"))))
        .collect();
    files.sort();
    let tests = extract::test_only(&files);
    if !tests.is_empty() {
        eprintln!("The log catalog reads only production sources; only tests compile these, so take them out of the crate's report_sources:");
        for file in &tests {
            eprintln!("  {}", file.display());
        }
        std::process::exit(1);
    }
    let files: Vec<(String, String)> = files.into_iter().map(|(file, source)| (file.to_string_lossy().into_owned(), source)).collect();
    let (catalog, vocabulary) = extract::fragment(&files, log, words);
    (catalog, vocabulary.into_iter().map(|word| word + "\n").collect())
}

fn join(fragments: &[String]) -> (String, String) {
    let read = |file: &String| std::fs::read_to_string(file).unwrap_or_else(|error| panic!("read {file}: {error}"));
    let catalogs = fragments.iter().filter(|file| file.ends_with(".catalog")).map(|file| (read(file), Default::default()));
    let words = fragments.iter().filter(|file| file.ends_with(".words")).map(|file| (String::new(), read(file).lines().map(str::to_owned).collect()));
    extract::join(catalogs.chain(words).collect::<Vec<_>>())
}
