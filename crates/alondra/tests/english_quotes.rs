//! English source strings quote with “ ”; « » belong to the Spanish halves only. This reads the
//! English half of every `tr!` and `trf!` in the crate.

use std::path::Path;

/// The first string literal after each `tr!(` and `trf!(`.
fn english_halves(text: &str) -> Vec<String> {
    let mut found = vec![];
    for opener in ["tr!(", "trf!("] {
        for (at, _) in text.match_indices(opener) {
            let rest = text[at + opener.len()..].trim_start();
            let Some(body) = rest.strip_prefix('"') else { continue };
            let mut literal = String::new();
            let mut escaped = false;
            for c in body.chars() {
                match c {
                    '"' if !escaped => break,
                    '\\' if !escaped => escaped = true,
                    _ => {
                        escaped = false;
                        literal.push(c);
                    }
                }
            }
            found.push(literal);
        }
    }
    found
}

fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display())) {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn english_halves_quote_with_curly_quotes() {
    let mut files = vec![];
    sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    let offending: Vec<String> = files
        .iter()
        .flat_map(|path| {
            let text = std::fs::read_to_string(path).unwrap();
            english_halves(&text).into_iter().filter(|s| s.contains('«') || s.contains('»')).map(move |s| format!("{}: {s}", path.display()))
        })
        .collect();
    assert!(offending.is_empty(), "English halves with « »:\n{}", offending.join("\n"));
}

#[test]
fn the_english_half_is_the_first_literal_even_on_the_next_line() {
    let text = "trf!(\n    \"after “{t}”\",\n    \"tras «{t}»\"\n) tr!(\"say \\\"hi\\\"\", \"di «hola»\")";
    assert_eq!(english_halves(text), ["say \"hi\"", "after “{t}”"]);
}
