//! One language per run: in the modules a person reads, every clap `about`/`help` and every
//! `bail!` message goes through `tr!`/`trf!`. This reads those sources, test code left out.

use std::path::{Path, PathBuf};

const USER_FACING: &[&str] = &[
    "main.rs",
    "delivery.rs",
    "doctor",
    "update.rs",
    "help.rs",
    "version.rs",
    "setup.rs",
    "claude_auth.rs",
    "service_setup.rs",
    "service_setup",
    "service_pair.rs",
    "setup_projects.rs",
];

fn sources(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display())) {
            sources(&entry.unwrap().path(), out);
        }
    } else if path.extension().is_some_and(|e| e == "rs") {
        out.push(path.to_path_buf());
    }
}

/// The source with every `#[cfg(test)]` item blanked out, line breaks kept so lines still count.
fn without_tests(text: &str) -> String {
    let mut out: Vec<char> = text.chars().collect();
    let marker: Vec<char> = "#[cfg(test)]".chars().collect();
    let mut i = 0;
    while i + marker.len() <= out.len() {
        if out[i..i + marker.len()] != marker[..] {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i + marker.len();
        let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
        while j < out.len() {
            let c = out[j];
            if in_string {
                match c {
                    '\\' if !escaped => escaped = true,
                    '"' if !escaped => in_string = false,
                    _ => escaped = false,
                }
            } else {
                match c {
                    '\'' if out.get(j + 2) == Some(&'\'') => j += 2,
                    '\'' if out.get(j + 1) == Some(&'\\') && out.get(j + 3) == Some(&'\'') => j += 3,
                    '"' => in_string = true,
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    ';' if depth == 0 => break,
                    _ => {}
                }
            }
            j += 1;
        }
        let end = j.min(out.len() - 1);
        for c in &mut out[start..=end] {
            if *c != '\n' {
                *c = ' ';
            }
        }
        i = j + 1;
    }
    out.into_iter().collect()
}

fn literal_after(rest: &str) -> Option<String> {
    let body = rest.trim_start().strip_prefix('"')?;
    let mut literal = String::new();
    let mut escaped = false;
    for c in body.chars() {
        match c {
            '"' if !escaped => return Some(literal),
            '\\' if !escaped => escaped = true,
            _ => {
                escaped = false;
                literal.push(c);
            }
        }
    }
    None
}

/// Whether a literal has words in it, not only `{}` placeholders.
fn has_words(literal: &str) -> bool {
    let mut depth = 0;
    literal.chars().any(|c| {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => return depth == 0 && c.is_alphabetic(),
        }
        false
    })
}

/// `bail!("…")` with words, and `about`/`help` attributes set to a literal.
fn literal_texts(text: &str) -> Vec<(usize, String)> {
    let code = without_tests(text);
    let mut found = vec![];
    for (number, line) in code.lines().enumerate() {
        for (at, _) in line.match_indices("bail!(") {
            if let Some(literal) = literal_after(&line[at + "bail!(".len()..]).filter(|l| has_words(l)) {
                found.push((number + 1, format!("bail!(\"{literal}\")")));
            }
        }
        for key in ["about", "long_about", "help", "long_help"] {
            for (at, _) in line.match_indices(key) {
                let before = line[..at].trim_end();
                let after = line[at + key.len()..].trim_start();
                let attribute = before.ends_with('(') || before.ends_with(',');
                if let Some(value) = after.strip_prefix('=').filter(|v| !v.starts_with('=')) {
                    if let Some(literal) = literal_after(value).filter(|_| attribute) {
                        found.push((number + 1, format!("{key} = \"{literal}\"")));
                    }
                }
            }
        }
    }
    found
}

#[test]
fn user_facing_texts_go_through_tr() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![];
    for name in USER_FACING {
        sources(&src.join(name), &mut files);
    }
    assert!(files.len() >= USER_FACING.len(), "a user-facing module moved: {files:?}");
    let mut offenders = vec![];
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for (line, what) in literal_texts(&text) {
            offenders.push(format!("{}:{line}: {what}", file.display()));
        }
    }
    assert!(offenders.is_empty(), "wrap these in tr!(\"English\", \"Español\") or trf!:\n{}", offenders.join("\n"));
}

#[test]
fn the_scan_finds_literals_and_skips_tests_and_translated_texts() {
    let source = r#"
#[command(about = "Literal about")]
#[arg(long, help = "Literal help")]
#[arg(long, help = tr!("Fine", "Bien"))]
fn f() -> Result<()> { bail!("plain words") }
fn g() -> Result<()> { bail!(tr!("fine", "bien")) }
fn h() -> Result<()> { bail!("{}", words) }
fn i() { let help = "not an attribute"; }
#[cfg(test)]
mod tests {
    fn t() -> Result<()> { bail!("injected {failure}") }
}
#[cfg(test)]
fn helper() { bail!("also a test") }
"#;
    let found: Vec<String> = literal_texts(source).into_iter().map(|(line, what)| format!("{line} {what}")).collect();
    assert_eq!(
        found,
        ["2 about = \"Literal about\"", "3 help = \"Literal help\"", "5 bail!(\"plain words\")"]
    );
}
