//! Writes the catalog of the daemon's log lines and the words of the code into `OUT_DIR`; the
//! crash report keeps only the log text that comes from them (`crates/cli-report/src/report/templates.rs`). Bazel
//! writes both in their own action (`log_catalog.rs`) and hands them over to be copied.
#[path = "../cli-report/src/report/extract.rs"]
#[allow(dead_code)]
mod extract;

fn main() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    if std::env::var_os("ALONDRA_LOG_CATALOG_GENERATED").is_some() {
        for file in ["log-catalog.txt", "vocabulary.txt"] {
            std::fs::copy(manifest.join(file), out.join(file)).expect("copy the generated catalog");
        }
        return;
    }
    let root = manifest.join("../..");
    for dir in extract::LOG_SOURCES.iter().chain(extract::VOCABULARY_SOURCES) {
        println!("cargo:rerun-if-changed={}", root.join(dir).display());
    }
    println!("cargo:rerun-if-changed=../cli-report/src/report/extract.rs");
    let (catalog, vocabulary) = extract::build(&root);
    std::fs::write(out.join("log-catalog.txt"), catalog).expect("write the log catalog");
    std::fs::write(out.join("vocabulary.txt"), vocabulary).expect("write the vocabulary");
}
