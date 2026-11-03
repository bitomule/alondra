use super::*;
use std::path::PathBuf;

/// Fixtures `parse` still gets wrong. Remove a name when its fixture starts passing.
const KNOWN_BROKEN_TUI: &[&str] = &[];

pub(super) fn fixtures_dir() -> PathBuf {
    match std::env::var("TEST_SRCDIR") {
        Ok(root) => PathBuf::from(root)
            .join(std::env::var("TEST_WORKSPACE").unwrap_or_else(|_| "_main".into()))
            .join("fixtures/markdown"),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/markdown"),
    }
}

fn fixtures() -> Vec<(String, String, String)> {
    let dir = fixtures_dir();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| {
            e.ok()?
                .file_name()
                .to_str()?
                .strip_suffix(".md")
                .map(str::to_string)
        })
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let read = |ext: &str| {
                std::fs::read_to_string(dir.join(format!("{name}.{ext}")))
                    .unwrap_or_else(|e| panic!("{name}.{ext}: {e}"))
            };
            (name.clone(), read("md"), read("blocks"))
        })
        .collect()
}

fn escaped(s: &str) -> String {
    s.replace('\n', "\\n")
}

fn outline(blocks: &[Block]) -> Vec<String> {
    let mut out = vec![];
    let mut row = 0;
    for b in blocks {
        let (depth, text) = (b.depth, escaped(&b.plain()));
        let line = match &b.kind {
            Kind::Heading(level) => format!("heading:{level} {depth} | {text}"),
            Kind::Para | Kind::Label => format!("para {depth} | {text}"),
            Kind::Item(marker) => {
                let marker = match marker.as_str() {
                    "•" => "-",
                    "☐" => "[ ]",
                    "☑" => "[x]",
                    m => m,
                };
                format!("item {depth} | {marker} {text}")
            }
            Kind::Code(lang) => {
                let code = b
                    .lines
                    .iter()
                    .map(|l| plain(l))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "code{} {depth} | {}",
                    lang.as_ref().map(|l| format!(":{l}")).unwrap_or_default(),
                    escaped(&code)
                )
            }
            Kind::Quote => format!("quote {depth} | {text}"),
            Kind::Row { header, aligns } => {
                let joined = b
                    .lines
                    .iter()
                    .map(|c| escaped(&one_line(&plain(c))))
                    .collect::<Vec<_>>()
                    .join(" · ");
                if *header {
                    row = 0;
                    let aligns: String = aligns
                        .iter()
                        .map(|a| match a {
                            Alignment::Right => 'r',
                            Alignment::Center => 'c',
                            Alignment::Left | Alignment::None => 'l',
                        })
                        .collect();
                    format!("head:{aligns} {depth} | {joined}")
                } else {
                    row += 1;
                    format!("row {row} | {joined}")
                }
            }
            Kind::Rule => format!("rule {depth} |"),
            Kind::DiffFiles { .. } | Kind::DiffEntry(_) | Kind::DiffFile(_) | Kind::DiffLine(_) => {
                format!("diff {depth} | {text}")
            }
            Kind::Image { cols, rows } => format!("image:{cols}x{rows} {depth} | {text}"),
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
    assert!(
        fixtures.len() >= 16,
        "found {} fixtures in {}",
        fixtures.len(),
        fixtures_dir().display()
    );
    for name in KNOWN_BROKEN_TUI {
        assert!(
            fixtures.iter().any(|(n, ..)| n == name),
            "{name} is listed as broken but has no fixture"
        );
    }
    let mut wrong = vec![];
    for (name, md, blocks) in &fixtures {
        let expected: Vec<String> = blocks.lines().map(|l| l.trim_end().to_string()).collect();
        let actual = outline(&parse(md));
        let broken = KNOWN_BROKEN_TUI.contains(&name.as_str());
        match (actual == expected, broken) {
            (true, true) => wrong.push(format!(
                "{name} passes now: remove it from KNOWN_BROKEN_TUI"
            )),
            (false, false) => wrong.push(format!(
                "{name}\n  expected:\n    {}\n  actual:\n    {}",
                expected.join("\n    "),
                actual.join("\n    ")
            )),
            _ => {}
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n\n"));
}

const WIDTHS: [usize; 3] = [40, 80, 120];

/// Table fixtures and the mode their first table takes at 40, 80 and 120 columns.
const TABLE_MODES: &[(&str, [Mode; 3])] = &[
    (
        "card_redraft_links_table",
        [Mode::Wrapped, Mode::Wrapped, Mode::Wrapped],
    ),
    ("card_tabla", [Mode::Records, Mode::Wrapped, Mode::Wrapped]),
    ("table_after_para", [Mode::Grid, Mode::Grid, Mode::Grid]),
    ("table_align_code", [Mode::Grid, Mode::Grid, Mode::Grid]),
    ("table_no_outer_pipes", [Mode::Grid, Mode::Grid, Mode::Grid]),
];

fn snapshot(blocks: &[Block], width: usize) -> String {
    let lines: Vec<String> = render(blocks, width)
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    lines.join("\n") + "\n"
}

#[test]
fn table_fixtures_render_to_their_snapshots_at_40_80_and_120_columns() {
    let update = std::env::var_os("UPDATE_SNAPSHOTS").is_some();
    let tables: Vec<(String, Vec<Block>)> = fixtures()
        .into_iter()
        .map(|(name, md, _)| (name, parse(&md)))
        .filter(|(_, b)| b.iter().any(|b| matches!(b.kind, Kind::Row { .. })))
        .collect();
    let listed: Vec<&str> = TABLE_MODES.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        tables.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        listed,
        "every table fixture needs its modes in TABLE_MODES"
    );
    let mut wrong = vec![];
    for ((name, blocks), (_, modes)) in tables.iter().zip(TABLE_MODES) {
        let first = blocks
            .iter()
            .position(|b| matches!(b.kind, Kind::Row { .. }))
            .expect("a table fixture has rows");
        for (width, mode) in WIDTHS.into_iter().zip(modes) {
            let actual = snapshot(blocks, width);
            if let Some(l) = actual.lines().find(|l| str_width(l) > width) {
                wrong.push(format!(
                    "{name} at {width}: line wider than the screen: {l:?}"
                ));
            }
            let got = table_at(blocks, first, width).mode;
            if got != *mode {
                wrong.push(format!("{name} at {width}: {got:?}, expected {mode:?}"));
            }
            let file = format!("tables/{name}.{width}.txt");
            if update {
                let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../fixtures/markdown/tables");
                std::fs::create_dir_all(&dir).expect("create the snapshot folder");
                std::fs::write(dir.join(format!("{name}.{width}.txt")), &actual)
                    .expect("write the snapshot");
                continue;
            }
            match std::fs::read_to_string(fixtures_dir().join(&file)) {
                Ok(expected) if expected == actual => {}
                Ok(expected) => wrong.push(format!(
                    "{file} differs\n  expected:\n{expected}\n  actual:\n{actual}"
                )),
                Err(e) => wrong.push(format!(
                    "{file}: {e} (UPDATE_SNAPSHOTS=1 cargo test writes it)"
                )),
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n\n"));
}
