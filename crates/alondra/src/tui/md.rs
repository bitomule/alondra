use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub const QUOTE_MAX: usize = 200;
const CODE: Color = Color::Indexed(180);
const ACCENT: Color = Color::LightYellow;
const DIM: Color = Color::DarkGray;

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Heading(u8),
    Para,
    Item { depth: usize, marker: String },
    Code,
    Quote,
    Row { header: bool },
    Rule,
    Label,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub kind: Kind,
    pub lines: Vec<String>,
}

impl Block {
    fn new(kind: Kind, line: &str) -> Self {
        Block { kind, lines: vec![line.to_string()] }
    }

    pub fn label(text: &str) -> Self {
        Block::new(Kind::Label, text)
    }

    pub fn para(text: &str) -> Self {
        Block::new(Kind::Para, text)
    }

    pub fn plain(&self) -> String {
        let joined = match self.kind {
            Kind::Row { .. } => cells(&self.lines[0]).join(" | "),
            _ => self.lines.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" "),
        };
        strip_inline(&joined).split_whitespace().collect::<Vec<_>>().join(" ")
    }

    pub fn quote(&self) -> String {
        clip(&self.plain(), QUOTE_MAX)
    }

    pub fn commentable(&self) -> bool {
        !matches!(self.kind, Kind::Rule | Kind::Label)
    }
}

pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>()) }
}

pub fn parse(text: &str) -> Vec<Block> {
    let mut out = vec![];
    let mut cur: Option<Block> = None;
    let mut fence: Option<&str> = None;
    for raw in text.lines() {
        let line = raw.trim_end();
        let t = line.trim_start();
        if let Some(f) = fence {
            if t.starts_with(f) {
                fence = None;
                flush(&mut cur, &mut out);
            } else if let Some(b) = cur.as_mut() {
                b.lines.push(line.replace('\t', "    "));
            }
            continue;
        }
        let depth = (line.len() - t.len()) / 2;
        if t.starts_with("```") || t.starts_with("~~~") {
            flush(&mut cur, &mut out);
            fence = Some(if t.starts_with("```") { "```" } else { "~~~" });
            cur = Some(Block { kind: Kind::Code, lines: vec![] });
            continue;
        }
        if t.is_empty() {
            flush(&mut cur, &mut out);
            continue;
        }
        if let Some(level) = heading_level(t) {
            flush(&mut cur, &mut out);
            out.push(Block::new(Kind::Heading(level as u8), t[level..].trim().trim_end_matches('#').trim_end()));
            continue;
        }
        if is_rule(t) {
            flush(&mut cur, &mut out);
            out.push(Block { kind: Kind::Rule, lines: vec![] });
            continue;
        }
        if t.starts_with('|') {
            flush(&mut cur, &mut out);
            if is_table_separator(t) {
                if let Some(Block { kind: Kind::Row { header }, .. }) = out.last_mut() {
                    *header = true;
                }
            } else {
                out.push(Block::new(Kind::Row { header: false }, t));
            }
            continue;
        }
        if let Some(rest) = t.strip_prefix('>') {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            match cur.as_mut() {
                Some(b) if b.kind == Kind::Quote => b.lines.push(rest.to_string()),
                _ => {
                    flush(&mut cur, &mut out);
                    cur = Some(Block::new(Kind::Quote, rest));
                }
            }
            continue;
        }
        if let Some((marker, rest)) = list_marker(t) {
            flush(&mut cur, &mut out);
            cur = Some(Block::new(Kind::Item { depth, marker }, rest));
            continue;
        }
        match cur.as_mut() {
            Some(b) if matches!(b.kind, Kind::Para | Kind::Item { .. } | Kind::Quote) => b.lines.push(t.to_string()),
            _ => {
                flush(&mut cur, &mut out);
                cur = Some(Block::para(t));
            }
        }
    }
    flush(&mut cur, &mut out);
    out
}

fn flush(cur: &mut Option<Block>, out: &mut Vec<Block>) {
    if let Some(b) = cur.take() {
        out.push(b);
    }
}

fn heading_level(t: &str) -> Option<usize> {
    let level = t.chars().take_while(|&c| c == '#').count();
    ((1..=6).contains(&level) && t[level..].starts_with(' ')).then_some(level)
}

fn is_rule(t: &str) -> bool {
    let chars: Vec<char> = t.chars().filter(|c| !c.is_whitespace()).collect();
    chars.len() >= 3 && ['-', '*', '_'].iter().any(|&m| chars.iter().all(|&c| c == m))
}

fn is_table_separator(t: &str) -> bool {
    t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

fn list_marker(t: &str) -> Option<(String, &str)> {
    let bullet = ["- ", "* ", "+ "].iter().find_map(|m| t.strip_prefix(m));
    if let Some(rest) = bullet {
        let rest = rest.trim_start();
        for (box_, mark) in [("[ ] ", "☐"), ("[x] ", "☑"), ("[X] ", "☑")] {
            if let Some(r) = rest.strip_prefix(box_) {
                return Some((mark.into(), r));
            }
        }
        return Some(("•".into(), rest));
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    let after = &t[digits..];
    let rest = after.strip_prefix(". ").or_else(|| after.strip_prefix(") "))?;
    Some((format!("{}.", &t[..digits]), rest.trim_start()))
}

pub fn cells(row: &str) -> Vec<String> {
    let t = row.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().to_string()).collect()
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Fmt {
    Plain,
    Bold,
    Italic,
    Code,
    Link,
}

fn find(chars: &[char], from: usize, pat: &[char]) -> Option<usize> {
    if chars.len() < pat.len() {
        return None;
    }
    (from..=chars.len() - pat.len()).find(|&j| chars[j..j + pat.len()] == *pat)
}

fn inline_parts(s: &str) -> Vec<(String, Fmt)> {
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<(String, Fmt)> = vec![];
    let mut buf = String::new();
    let mut i = 0;
    let emit = |buf: &mut String, out: &mut Vec<(String, Fmt)>, text: String, fmt: Fmt| {
        if !buf.is_empty() {
            out.push((std::mem::take(buf), Fmt::Plain));
        }
        out.push((text, fmt));
    };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '\\'
            && let Some(n) = next.filter(char::is_ascii_punctuation)
        {
            buf.push(n);
            i += 2;
            continue;
        }
        if c == '`'
            && let Some(end) = find(&chars, i + 1, &['`'])
        {
            emit(&mut buf, &mut out, chars[i + 1..end].iter().collect(), Fmt::Code);
            i = end + 1;
            continue;
        }
        if (c == '*' || c == '_')
            && next == Some(c)
            && let Some(end) = find(&chars, i + 2, &[c, c])
            && end > i + 2
        {
            emit(&mut buf, &mut out, chars[i + 2..end].iter().collect(), Fmt::Bold);
            i = end + 2;
            continue;
        }
        if c == '*'
            && next.is_some_and(|n| !n.is_whitespace() && n != '*')
            && let Some(end) = find(&chars, i + 1, &['*'])
            && !chars[end - 1].is_whitespace()
        {
            emit(&mut buf, &mut out, chars[i + 1..end].iter().collect(), Fmt::Italic);
            i = end + 1;
            continue;
        }
        if c == '['
            && let Some(close) = find(&chars, i + 1, &[']'])
            && chars.get(close + 1) == Some(&'(')
            && let Some(paren) = find(&chars, close + 2, &[')'])
        {
            emit(&mut buf, &mut out, chars[i + 1..close].iter().collect(), Fmt::Link);
            i = paren + 1;
            continue;
        }
        buf.push(c);
        i += 1;
    }
    if !buf.is_empty() {
        out.push((buf, Fmt::Plain));
    }
    out
}

pub fn strip_inline(s: &str) -> String {
    inline_parts(s).into_iter().map(|(t, fmt)| if matches!(fmt, Fmt::Plain | Fmt::Code) { t } else { strip_inline(&t) }).collect()
}

pub fn inline(s: &str, base: Style) -> Vec<Span<'static>> {
    inline_parts(s)
        .into_iter()
        .flat_map(|(t, fmt)| match fmt {
            Fmt::Plain => vec![Span::styled(t, base)],
            Fmt::Code => vec![Span::styled(t, base.fg(CODE))],
            Fmt::Bold => inline(&t, base.add_modifier(Modifier::BOLD)),
            Fmt::Italic => inline(&t, base.add_modifier(Modifier::ITALIC)),
            Fmt::Link => inline(&t, base.fg(Color::LightBlue).add_modifier(Modifier::UNDERLINED)),
        })
        .collect()
}

pub fn char_width(c: char) -> usize {
    let mut b = [0u8; 4];
    Span::raw(&*c.encode_utf8(&mut b)).width()
}

pub fn str_width(s: &str) -> usize {
    Span::raw(s).width()
}

pub fn truncate(s: &str, max: usize) -> String {
    if str_width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = char_width(c);
        if w + cw + 1 > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn prefix_width(p: &[Span]) -> usize {
    p.iter().map(Span::width).sum()
}

fn close_line(mut line: Vec<Span<'static>>) -> Line<'static> {
    while line.last().is_some_and(|s| !s.content.is_empty() && s.content.trim().is_empty()) {
        line.pop();
    }
    Line::from(line)
}

pub fn wrap(spans: Vec<Span<'static>>, width: usize, first: Vec<Span<'static>>, rest: Vec<Span<'static>>) -> Vec<Line<'static>> {
    let width = width.max(4);
    let mut tokens: Vec<(String, Style, bool)> = vec![];
    for s in spans {
        let mut cur = String::new();
        let mut space: Option<bool> = None;
        for ch in s.content.chars() {
            let sp = ch.is_whitespace();
            if space.is_some_and(|x| x != sp) {
                tokens.push((std::mem::take(&mut cur), s.style, !sp));
            }
            cur.push(if sp { ' ' } else { ch });
            space = Some(sp);
        }
        if let Some(sp) = space {
            tokens.push((cur, s.style, sp));
        }
    }
    let mut lines = vec![];
    let mut line = first.clone();
    let mut w = prefix_width(&first);
    let mut empty = true;
    for (text, style, space) in tokens {
        let tw = str_width(&text);
        if space {
            if empty {
                continue;
            }
            if w + tw < width {
                line.push(Span::styled(text, style));
                w += tw;
            } else {
                lines.push(close_line(std::mem::replace(&mut line, rest.clone())));
                w = prefix_width(&rest);
                empty = true;
            }
            continue;
        }
        if w + tw > width && !empty {
            lines.push(close_line(std::mem::replace(&mut line, rest.clone())));
            w = prefix_width(&rest);
        }
        if w + tw <= width {
            line.push(Span::styled(text, style));
            w += tw;
            empty = false;
            continue;
        }
        let mut chunk = String::new();
        for ch in text.chars() {
            let cw = char_width(ch);
            if w + cw > width && !chunk.is_empty() {
                line.push(Span::styled(std::mem::take(&mut chunk), style));
                lines.push(close_line(std::mem::replace(&mut line, rest.clone())));
                w = prefix_width(&rest);
            }
            chunk.push(ch);
            w += cw;
        }
        line.push(Span::styled(chunk, style));
        empty = false;
    }
    lines.push(close_line(line));
    lines
}

fn hard_wrap(text: &str, width: usize, prefix: Span<'static>, style: Style) -> Vec<Line<'static>> {
    let avail = width.saturating_sub(prefix.width()).max(1);
    let mut out = vec![];
    let mut cur = String::new();
    let mut w = 0;
    for ch in text.chars() {
        let cw = char_width(ch);
        if w + cw > avail {
            out.push(Line::from(vec![prefix.clone(), Span::styled(std::mem::take(&mut cur), style)]));
            w = 0;
        }
        cur.push(ch);
        w += cw;
    }
    out.push(Line::from(vec![prefix, Span::styled(cur, style)]));
    out
}

pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub ranges: Vec<(usize, usize)>,
}

fn joins_previous(prev: &Kind, cur: &Kind) -> bool {
    matches!((prev, cur), (Kind::Item { .. }, Kind::Item { .. }) | (Kind::Row { .. }, Kind::Row { .. }))
}

#[derive(Default)]
struct Table {
    widths: Vec<usize>,
    header: Option<Vec<String>>,
}

fn table_at(blocks: &[Block], start: usize) -> Table {
    let header = match &blocks[start] {
        Block { kind: Kind::Row { header: true }, lines } => Some(cells(&lines[0])),
        _ => None,
    };
    Table { widths: table_widths(blocks, start), header }
}

fn table_widths(blocks: &[Block], start: usize) -> Vec<usize> {
    let mut widths: Vec<usize> = vec![];
    for b in blocks[start..].iter().take_while(|b| matches!(b.kind, Kind::Row { .. })) {
        for (i, c) in cells(&b.lines[0]).iter().enumerate() {
            let w = str_width(&strip_inline(c));
            if i >= widths.len() {
                widths.push(w);
            } else {
                widths[i] = widths[i].max(w);
            }
        }
    }
    widths
}

pub fn render(blocks: &[Block], width: usize) -> Rendered {
    let width = width.max(8);
    let mut lines: Vec<Line<'static>> = vec![];
    let mut ranges = vec![];
    let mut table = Table::default();
    for (i, b) in blocks.iter().enumerate() {
        if i > 0 && !joins_previous(&blocks[i - 1].kind, &b.kind) {
            lines.push(Line::default());
        }
        if matches!(b.kind, Kind::Row { .. }) && (i == 0 || !matches!(blocks[i - 1].kind, Kind::Row { .. })) {
            table = table_at(blocks, i);
        }
        let start = lines.len();
        lines.extend(render_block(b, width, &table));
        ranges.push((start, lines.len()));
    }
    Rendered { lines, ranges }
}

/// A table too wide for the screen reads as one record per row, each cell under its column name.
fn record(cs: &[String], header: Option<&[String]>, width: usize) -> Vec<Line<'static>> {
    let mut out = vec![];
    for (i, c) in cs.iter().enumerate() {
        let lead = if i == 0 { "▸ " } else { "  " };
        let mut first = vec![Span::styled(lead, Style::new().fg(ACCENT))];
        if let Some(name) = header.and_then(|h| h.get(i)).filter(|n| !n.trim().is_empty()) {
            first.push(Span::styled(format!("{}: ", strip_inline(name)), Style::new().fg(DIM).add_modifier(Modifier::BOLD)));
        }
        out.extend(wrap(inline(c, Style::new()), width, first, vec![Span::raw("    ")]));
    }
    out
}

fn render_block(b: &Block, width: usize, table: &Table) -> Vec<Line<'static>> {
    let text = b.lines.join(" ");
    match &b.kind {
        Kind::Heading(level) => {
            let style = match level {
                1 => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                2 => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                _ => Style::new().add_modifier(Modifier::BOLD),
            };
            wrap(inline(&text, style), width, vec![], vec![])
        }
        Kind::Para => wrap(inline(&text, Style::new()), width, vec![], vec![]),
        Kind::Item { depth, marker } => {
            let indent = "  ".repeat(*depth);
            let color = if marker.ends_with('.') { ACCENT } else { DIM };
            let first = vec![Span::raw(indent.clone()), Span::styled(format!("{marker} "), Style::new().fg(color))];
            let rest = vec![Span::raw(format!("{indent}{}", " ".repeat(str_width(marker) + 1)))];
            wrap(inline(&text, Style::new()), width, first, rest)
        }
        Kind::Quote => {
            let bar = || vec![Span::styled("▎ ", Style::new().fg(DIM))];
            wrap(inline(&text, Style::new().fg(Color::Gray).add_modifier(Modifier::ITALIC)), width, bar(), bar())
        }
        Kind::Code => {
            let prefix = Span::styled("│ ", Style::new().fg(DIM));
            let lines = if b.lines.is_empty() { vec![String::new()] } else { b.lines.clone() };
            lines.iter().flat_map(|l| hard_wrap(l, width, prefix.clone(), Style::new().fg(CODE))).collect()
        }
        Kind::Row { header } => {
            let cs = cells(&b.lines[0]);
            let widths = &table.widths;
            let total: usize = widths.iter().sum::<usize>() + 3 * widths.len().saturating_sub(1);
            let base = if *header { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
            if total > width || widths.len() < cs.len() {
                return match (header, table.header.as_deref()) {
                    (true, Some(_)) => vec![],
                    (_, names) => record(&cs, names, width),
                };
            }
            let mut spans = vec![];
            for (i, c) in cs.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" │ ", Style::new().fg(DIM)));
                }
                let cell = inline(c, base);
                let w: usize = cell.iter().map(Span::width).sum();
                spans.extend(cell);
                spans.push(Span::raw(" ".repeat(widths[i].saturating_sub(w))));
            }
            let mut out = vec![Line::from(spans)];
            if *header {
                let sep = widths.iter().map(|w| "─".repeat(*w)).collect::<Vec<_>>().join("─┼─");
                out.push(Line::styled(sep, Style::new().fg(DIM)));
            }
            out
        }
        Kind::Rule => vec![Line::styled("─".repeat(width), Style::new().fg(DIM))],
        Kind::Label => {
            let head = format!("── {} ", truncate(&text, width.saturating_sub(4)));
            let fill = width.saturating_sub(str_width(&head));
            vec![Line::styled(format!("{head}{}", "─".repeat(fill)), Style::new().fg(Color::Cyan))]
        }
    }
}

pub fn feedback(comments: &[(String, String)], general: &str, author: &str) -> String {
    let mut out = format!("Comments from {author}:");
    for (i, (quote, text)) in comments.iter().enumerate() {
        let body = text.trim().lines().map(|l| format!("   {l}")).collect::<Vec<_>>().join("\n");
        out.push_str(&format!("\n\n{}. On \"{}\":\n{body}", i + 1, clip(quote, QUOTE_MAX)));
    }
    let general = general.trim();
    if !general.is_empty() {
        out.push_str("\n\n");
        out.push_str(general);
    }
    out
}

#[cfg(test)]
mod corpus;

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn parses_the_blocks_a_plan_is_made_of() {
        let md = "# Plan\n\nFirst paragraph\ncontinues here.\n\n- one\n  continues one\n- [x] two\n  1. nested\n\n```rust\nfn main() {}\n\n```\n\n> long\n> quote\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n---\n";
        let b = parse(md);
        let kinds: Vec<&Kind> = b.iter().map(|b| &b.kind).collect();
        assert_eq!(
            kinds,
            vec![
                &Kind::Heading(1),
                &Kind::Para,
                &Kind::Item { depth: 0, marker: "•".into() },
                &Kind::Item { depth: 0, marker: "☑".into() },
                &Kind::Item { depth: 1, marker: "1.".into() },
                &Kind::Code,
                &Kind::Quote,
                &Kind::Row { header: true },
                &Kind::Row { header: false },
                &Kind::Rule,
            ]
        );
        assert_eq!(b[1].plain(), "First paragraph continues here.");
        assert_eq!(b[2].plain(), "one continues one");
        assert_eq!(b[5].lines, vec!["fn main() {}".to_string(), String::new()]);
        assert_eq!(b[7].plain(), "a | b");
    }

    /// hive/aso-skills, 23 Sep: a three-column table with long cells showed as wrapped `a │ b │ c`.
    const WIDE: &str = "## Tabla\n\n| Lo que traen | ¿Lo tenemos? | ¿Cambia lo que haríamos? |\n|---|---|---|\n| **Fuente de datos = Appeeky** (API/MCP de pago, 8 $/mes) para volumen y rankings | No, y a propósito | **No.** Sin esa clave sus skills no tienen dato: recomiendan de memoria |\n| Fórmula de oportunidad | Parecido | **No.** Un número inventado ordena peor que un juicio explícito |\n";

    #[test]
    fn a_table_that_fits_is_a_grid_and_one_that_does_not_is_one_record_per_row() {
        let blocks = parse(WIDE);
        let wide: Vec<String> = render(&blocks, 200).lines.iter().map(text_of).collect();
        assert!(wide.iter().any(|l| l.starts_with("Lo que traen") && l.contains(" │ ¿Lo tenemos?")), "{wide:#?}");
        assert!(wide.iter().any(|l| l.contains("─┼─")));

        let narrow: Vec<String> = render(&blocks, 60).lines.iter().map(text_of).collect();
        assert!(narrow.iter().all(|l| !l.contains(" │ ") && str_width(l) <= 60), "{narrow:#?}");
        let records: Vec<&String> = narrow.iter().filter(|l| l.starts_with("▸ ")).collect();
        assert_eq!(records.len(), 2, "{narrow:#?}");
        assert!(records[0].starts_with("▸ Lo que traen: Fuente de datos"), "{narrow:#?}");
        assert!(narrow.iter().any(|l| l.starts_with("  ¿Lo tenemos?: No, y a propósito")), "{narrow:#?}");
        assert!(narrow.iter().any(|l| l.starts_with("  ¿Cambia lo que haríamos?: No.")), "{narrow:#?}");
    }

    #[test]
    fn a_wide_table_without_a_header_still_reads_row_by_row() {
        let narrow: Vec<String> = render(&parse("| aaaaaaaaaaaa | bbbbbbbbbbbbbbbb | cccccccccccccc |\n"), 20).lines.iter().map(text_of).collect();
        assert_eq!(narrow.iter().filter(|l| l.starts_with("▸ ")).count(), 1, "{narrow:#?}");
        assert!(narrow.iter().all(|l| !l.contains(" │ ")), "{narrow:#?}");
    }

    #[test]
    fn quote_drops_markdown_and_stops_at_200_chars() {
        let b = parse("Use **`cargo`** and [the guide](https://x.y) *now*.");
        assert_eq!(b[0].plain(), "Use cargo and the guide now.");
        let long = parse(&"word ".repeat(60))[0].quote();
        assert_eq!(long.chars().count(), QUOTE_MAX);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn snake_case_and_lone_asterisks_stay_literal() {
        assert_eq!(strip_inline("my_var_x and 2 * 3 * 4"), "my_var_x and 2 * 3 * 4");
        assert_eq!(strip_inline(r"52\. two \*not\*"), "52. two *not*");
    }

    #[test]
    fn feedback_has_the_exact_format_the_nodes_expect() {
        let f = feedback(&[("Step 2".into(), "Do it before\nstep 1".into()), ("Table".into(), "Not needed".into())], "Otherwise, fine.", "Ana");
        assert_eq!(
            f,
            "Comments from Ana:\n\n1. On \"Step 2\":\n   Do it before\n   step 1\n\n2. On \"Table\":\n   Not needed\n\nOtherwise, fine."
        );
        assert_eq!(feedback(&[], "  Just this  ", "Ana"), "Comments from Ana:\n\nJust this");
    }

    #[test]
    fn wrap_respects_width_prefixes_and_long_words() {
        let lines = wrap(vec![Span::raw("one two three four supercalifragilisticexpialidocious")], 12, vec![Span::raw("• ")], vec![Span::raw("  ")]);
        for l in &lines {
            assert!(l.width() <= 12, "{:?}", text_of(l));
        }
        assert_eq!(text_of(&lines[0]), "• one two");
        assert!(text_of(&lines[1]).starts_with("  three"));
    }

    #[test]
    fn ranges_cover_each_block_and_skip_the_gaps() {
        let blocks = parse("# T\n\none\n\n- a\n- b");
        let r = render(&blocks, 40);
        assert_eq!(r.ranges, vec![(0, 1), (2, 3), (4, 5), (5, 6)]);
        assert_eq!(r.lines.len(), 6);
    }

    #[test]
    fn truncate_counts_display_width() {
        assert_eq!(truncate("some words", 6), "some …");
        assert_eq!(truncate("📎📎📎", 5), "📎📎…");
        assert_eq!(truncate("short", 10), "short");
    }
}
