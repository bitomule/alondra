//! Writes the crash report's log catalog and vocabulary from the sources under the current
//! directory, as its own Bazel action: the CLI then recompiles only when one of them changes.
#[path = "src/report/extract.rs"]
#[allow(dead_code)]
mod extract;

fn main() {
    let mut outputs = std::env::args().skip(1);
    let (catalog, vocabulary) = extract::build(std::path::Path::new("."));
    std::fs::write(outputs.next().expect("the catalog path"), catalog).expect("write the log catalog");
    std::fs::write(outputs.next().expect("the vocabulary path"), vocabulary).expect("write the vocabulary");
}
