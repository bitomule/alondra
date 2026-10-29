//! What a node's hooks deduced (`states.rs`, the legacy `View`, `state_key`) is kept only for the
//! `claude --bg` nodes still handed over to the Agent SDK. The domain and the screens read a node
//! from its task's lifecycle: no code of theirs outside tests may name that deduction.

use std::path::{Path, PathBuf};

const DEDUCTION: &[&str] = &["crate::states", "states::", "crate::view::", "crate::View", "alondra_core::View", ".state_key", "node_reports"];

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display())) {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

/// The code without its `#[cfg(test)]` items: each runs from the attribute to the `;` ending its
/// line or to the closing brace at the attribute's indentation.
fn without_tests(text: &str) -> String {
    let mut code = String::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.trim() != "#[cfg(test)]" {
            code.push_str(line);
            code.push('\n');
            continue;
        }
        let close = format!("{}}}", &line[..line.len() - line.trim_start().len()]);
        let item = lines.by_ref().find(|line| !line.trim_start().starts_with("#[")).unwrap_or_default();
        if !item.trim_end().ends_with(';') && !item.trim_end().ends_with('}') {
            lines.by_ref().find(|line| line.trim_end() == close);
        }
    }
    code
}

fn is_test(path: &Path) -> bool {
    let name = path.file_name().unwrap().to_string_lossy();
    name == "tests.rs" || name.ends_with("_tests.rs") || path.components().any(|part| part.as_os_str() == "tests")
}

#[test]
fn test_items_are_left_out_and_the_rest_is_kept() {
    let text = "fn a() {}\n#[cfg(test)]\nfn b() {\n    x.state_key;\n}\n#[cfg(test)]\nuse y::states::Z;\n#[cfg(test)]\n#[path = \"t.rs\"]\nmod t;\nfn c() { node.state_key }\n";
    assert_eq!(without_tests(text), "fn a() {}\nfn c() { node.state_key }\n");
}

#[test]
fn the_domain_and_the_screens_never_name_what_hooks_deduced() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = vec![];
    for dir in ["src/domain", "src/views"] {
        sources(&root.join(dir), &mut files);
    }
    assert!(files.len() > 20, "the sources are in the runfiles: {files:?}");
    let named: Vec<String> = files.iter().filter(|path| !is_test(path)).flat_map(|path| {
        let text = std::fs::read_to_string(path).unwrap();
        let code = without_tests(&text);
        DEDUCTION.iter().filter(move |word| code.contains(**word)).map(move |word| format!("{} names {word}", path.display())).collect::<Vec<_>>()
    }).collect();
    assert!(named.is_empty(), "{named:#?}");
}
