//! Writes the catalog of the daemon's log lines and the words of the code into `OUT_DIR`; the
//! crash report keeps only the log text that comes from them (`src/report/templates.rs`).
#[path = "src/report/extract.rs"]
#[allow(dead_code)]
mod extract;

fn main() {
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..");
    for dir in extract::LOG_SOURCES.iter().chain(extract::VOCABULARY_SOURCES) {
        println!("cargo:rerun-if-changed={}", root.join(dir).display());
    }
    println!("cargo:rerun-if-changed=src/report/extract.rs");
    let (catalog, vocabulary) = extract::build(&root);
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    std::fs::write(out.join("log-catalog.txt"), catalog).expect("write the log catalog");
    std::fs::write(out.join("vocabulary.txt"), vocabulary).expect("write the vocabulary");
}
