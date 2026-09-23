use super::*;
use std::path::PathBuf;

/// Fixtures `parse` still gets wrong. Remove a name when its fixture starts passing.
const KNOWN_BROKEN_TUI: &[&str] = &[
    "code_in_list",
    "fence_long",
    "inline_mix",
    "nested_lists",
    "pipe_not_table",
    "quote_nested",
    "setext_headings",
    "table_align_code",
    "table_no_outer_pipes",
];

fn fixtures_dir() -> PathBuf {
    match std::env::var("TEST_SRCDIR") {
        Ok(root) => PathBuf::from(root).join(std::env::var("TEST_WORKSPACE").unwrap_or_else(|_| "_main".into())).join("fixtures/markdown"),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/markdown"),
    }
}

fn fixtures() -> Vec<(String, String, String)> {
    let dir = fixtures_dir();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".md").map(str::to_string))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let read = |ext: &str| std::fs::read_to_string(dir.join(format!("{name}.{ext}"))).unwrap_or_else(|e| panic!("{name}.{ext}: {e}"));
            (name.clone(), read("md"), read("blocks"))
        })
        .collect()
}

fn escaped(s: &str) -> String {
    s.replace('\n', "\\n")
}

fn cell_text(cell: &str) -> String {
    one_line(&strip_inline(cell))
}

fn outline(blocks: &[Block]) -> Vec<String> {
    let mut out = vec![];
    let mut row = 0;
    for b in blocks {
        let line = match &b.kind {
            Kind::Heading(level) => format!("heading:{level} 0 | {}", b.plain()),
            Kind::Para | Kind::Label => format!("para 0 | {}", b.plain()),
            Kind::Item { depth, marker } => {
                let marker = match marker.as_str() {
                    "•" => "-",
                    "☐" => "[ ]",
                    "☑" => "[x]",
                    m => m,
                };
                format!("item {depth} | {marker} {}", b.plain())
            }
            Kind::Code => format!("code 0 | {}", escaped(&b.lines.join("\n"))),
            Kind::Quote => format!("quote 0 | {}", b.plain()),
            Kind::Row { header } => {
                let cs: Vec<String> = cells(&b.lines[0]).iter().map(|c| cell_text(c)).collect();
                let joined = cs.join(" · ");
                if *header {
                    row = 0;
                    format!("head:{} 0 | {joined}", "l".repeat(cs.len()))
                } else {
                    row += 1;
                    format!("row {row} | {joined}")
                }
            }
            Kind::Rule => "rule 0 |".to_string(),
        };
        if !matches!(b.kind, Kind::Row { .. }) {
            row = 0;
        }
        out.push(line.trim_end().to_string());
    }
    out
}

#[test]
fn every_fixture_parses_to_its_outline_unless_known_broken() {
    let fixtures = fixtures();
    assert!(fixtures.len() >= 16, "found {} fixtures in {}", fixtures.len(), fixtures_dir().display());
    for name in KNOWN_BROKEN_TUI {
        assert!(fixtures.iter().any(|(n, ..)| n == name), "{name} is listed as broken but has no fixture");
    }
    let mut wrong = vec![];
    for (name, md, blocks) in &fixtures {
        let expected: Vec<String> = blocks.lines().map(|l| l.trim_end().to_string()).collect();
        let actual = outline(&parse(md));
        let broken = KNOWN_BROKEN_TUI.contains(&name.as_str());
        match (actual == expected, broken) {
            (true, true) => wrong.push(format!("{name} passes now: remove it from KNOWN_BROKEN_TUI")),
            (false, false) => wrong.push(format!("{name}\n  expected:\n    {}\n  actual:\n    {}", expected.join("\n    "), actual.join("\n    "))),
            _ => {}
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n\n"));
}
