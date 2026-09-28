use pulldown_cmark::{Alignment, CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub const QUOTE_MAX: usize = 200;
const CODE: Color = Color::Indexed(180);
const ACCENT: Color = Color::LightYellow;
const DIM: Color = Color::DarkGray;
const CELL_WORD_MAX: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Heading(u8),
    Para,
    Item(String),
    Code(Option<String>),
    Quote,
    Row { header: bool, aligns: Vec<Alignment> },
    Rule,
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Fmt {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    link: bool,
    host: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    text: String,
    fmt: Fmt,
}

pub type Text = Vec<Run>;

/// `lines` holds a text split at its hard breaks, the lines of a code block, or the cells of a table row.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub kind: Kind,
    pub depth: usize,
    pub lines: Vec<Text>,
}

impl Block {
    fn new(kind: Kind, depth: usize) -> Self {
        let lines = if matches!(kind, Kind::Row { .. } | Kind::Rule) { vec![] } else { vec![vec![]] };
        Block { kind, depth, lines }
    }


    pub fn plain(&self) -> String {
        let (sep, lines): (&str, Vec<String>) = match self.kind {
            Kind::Row { .. } => (" | ", self.lines.iter().map(|c| one_line(&plain(c))).collect()),
            Kind::Code(_) => (" ", self.lines.iter().map(|l| one_line(&plain(l))).collect()),
            _ => ("\n", self.lines.iter().map(|l| one_line(&plain(l))).collect()),
        };
        lines.join(sep).trim().to_string()
    }

    pub fn quote(&self) -> String {
        clip(&one_line(&self.plain()), QUOTE_MAX)
    }

    pub fn commentable(&self) -> bool {
        !matches!(self.kind, Kind::Rule)
    }
}

fn plain(text: &[Run]) -> String {
    text.iter().filter(|r| !r.fmt.host).map(|r| r.text.as_str()).collect()
}

pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>()) }
}

fn host(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?.rsplit('@').next()?;
    let host = host.strip_prefix("www.").unwrap_or(host);
    (!host.is_empty()).then_some(host)
}

#[derive(Default)]
struct Builder {
    out: Vec<Block>,
    cur: Option<Block>,
    item_text: bool,
    lists: Vec<Option<u64>>,
    quotes: usize,
    aligns: Vec<Alignment>,
    emphasis: usize,
    strong: usize,
    strike: usize,
    links: Vec<(String, usize)>,
    code: Option<String>,
}

impl Builder {
    fn nesting(&self) -> usize {
        self.lists.len() + self.quotes
    }

    fn flush(&mut self) {
        if let Some(b) = self.cur.take() {
            self.out.push(b);
        }
        self.item_text = false;
    }

    fn open(&mut self, kind: Kind, depth: usize) {
        self.flush();
        self.cur = Some(Block::new(kind, depth));
    }

    fn open_text(&mut self) {
        if self.item_text {
            self.item_text = false;
            return;
        }
        match self.quotes {
            0 => self.open(Kind::Para, self.nesting()),
            _ => self.open(Kind::Quote, self.nesting() - 1),
        }
    }

    fn current_line(&self) -> String {
        self.cur.as_ref().and_then(|b| b.lines.last()).map(|l| plain(l)).unwrap_or_default()
    }

    fn push(&mut self, text: &str, host: bool) {
        self.push_fmt(text, Fmt { host, ..Fmt::default() });
    }

    fn push_fmt(&mut self, text: &str, extra: Fmt) {
        if self.cur.is_none() {
            self.open_text();
        }
        let fmt = Fmt {
            bold: self.strong > 0,
            italic: self.emphasis > 0,
            strike: self.strike > 0,
            link: !self.links.is_empty() && !extra.host,
            ..extra
        };
        let Some(b) = self.cur.as_mut() else { return };
        if b.lines.is_empty() {
            b.lines.push(vec![]);
        }
        let line = b.lines.last_mut().expect("a line was just ensured");
        match line.last_mut() {
            Some(r) if r.fmt == fmt => r.text.push_str(text),
            _ => line.push(Run { text: text.to_string(), fmt }),
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => match self.code.as_mut() {
                Some(code) => code.push_str(&t),
                None => self.push(&t, false),
            },
            Event::Code(t) => self.push_fmt(&t, Fmt { code: true, ..Fmt::default() }),
            Event::InlineHtml(t) | Event::Html(t) | Event::InlineMath(t) | Event::DisplayMath(t) | Event::FootnoteReference(t) => self.push(&t, false),
            Event::SoftBreak => self.push(" ", false),
            Event::HardBreak => match self.cur.as_mut() {
                Some(b) if !matches!(b.kind, Kind::Row { .. }) => b.lines.push(vec![]),
                _ => self.push(" ", false),
            },
            Event::Rule => {
                self.open(Kind::Rule, self.nesting());
                self.flush();
            }
            Event::TaskListMarker(done) => {
                if let Some(Block { kind: Kind::Item(marker), .. }) = self.cur.as_mut() {
                    *marker = if done { "☑" } else { "☐" }.into();
                }
            }
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => self.open_text(),
            Tag::Heading { level, .. } => self.open(Kind::Heading(level as u8), self.nesting()),
            Tag::BlockQuote(_) => {
                self.flush();
                self.quotes += 1;
            }
            Tag::CodeBlock(kind) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next().map(str::to_string),
                    CodeBlockKind::Indented => None,
                };
                self.open(Kind::Code(lang), self.nesting());
                self.code = Some(String::new());
            }
            Tag::HtmlBlock => self.open(Kind::Para, self.nesting()),
            Tag::List(start) => {
                self.flush();
                self.lists.push(start);
            }
            Tag::Item => {
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{}.", *n - 1)
                    }
                    _ => "•".into(),
                };
                self.open(Kind::Item(marker), self.nesting().saturating_sub(1));
                self.item_text = true;
            }
            Tag::Table(aligns) => {
                self.flush();
                self.aligns = aligns;
            }
            Tag::TableHead | Tag::TableRow => self.open(Kind::Row { header: tag == Tag::TableHead, aligns: self.aligns.clone() }, self.nesting()),
            Tag::TableCell => {
                if let Some(b) = self.cur.as_mut() {
                    b.lines.push(vec![]);
                }
            }
            Tag::Emphasis => self.emphasis += 1,
            Tag::Strong => self.strong += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { link_type, dest_url, .. } | Tag::Image { link_type, dest_url, .. } => {
                let dest = if matches!(link_type, LinkType::Autolink | LinkType::Email) { String::new() } else { dest_url.to_string() };
                self.links.push((dest, self.current_line().len()));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::HtmlBlock | TagEnd::Item | TagEnd::TableHead | TagEnd::TableRow => self.flush(),
            TagEnd::CodeBlock => {
                let code = self.code.take().unwrap_or_default();
                let code = code.strip_suffix('\n').unwrap_or(&code);
                if let Some(b) = self.cur.as_mut() {
                    let fmt = Fmt { code: true, ..Fmt::default() };
                    b.lines = code.split('\n').map(|l| vec![Run { text: l.replace('\t', "    "), fmt }]).collect();
                }
                self.flush();
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quotes = self.quotes.saturating_sub(1);
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::Emphasis => self.emphasis = self.emphasis.saturating_sub(1),
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link | TagEnd::Image => {
                let Some((dest, at)) = self.links.pop() else { return };
                let line = self.current_line();
                let text = line.get(at..).unwrap_or_default().trim();
                if let Some(h) = host(&dest).filter(|h| text != dest && text != *h) {
                    self.push(&format!(" ({h})"), true);
                }
            }
            _ => {}
        }
    }
}

pub fn parse(text: &str) -> Vec<Block> {
    let mut b = Builder::default();
    for event in Parser::new_ext(text, Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH) {
        b.event(event);
    }
    b.flush();
    b.out
}

fn style(fmt: Fmt, base: Style) -> Style {
    let mut s = base;
    for (on, m) in [(fmt.bold, Modifier::BOLD), (fmt.italic, Modifier::ITALIC), (fmt.strike, Modifier::CROSSED_OUT)] {
        if on {
            s = s.add_modifier(m);
        }
    }
    if fmt.code {
        s = s.fg(CODE);
    }
    if fmt.link {
        s = s.fg(Color::LightBlue).add_modifier(Modifier::UNDERLINED);
    }
    if fmt.host {
        s = s.fg(DIM);
    }
    s
}

fn spans(text: &[Run], base: Style, hosts: bool) -> Vec<Span<'static>> {
    text.iter().filter(|r| hosts || !r.fmt.host).map(|r| Span::styled(r.text.clone(), style(r.fmt, base))).collect()
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
    matches!((prev, cur), (Kind::Item(_), Kind::Item(_)) | (Kind::Row { .. }, Kind::Row { .. }))
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub enum Mode {
    #[default]
    Grid,
    Wrapped,
    Records,
}

#[derive(Default)]
struct Table {
    widths: Vec<usize>,
    header: Option<Vec<String>>,
    mode: Mode,
    ruled: bool,
}

fn table_at(blocks: &[Block], start: usize, width: usize) -> Table {
    let rows: Vec<&Block> = blocks[start..].iter().take_while(|b| matches!(b.kind, Kind::Row { .. })).collect();
    let columns = rows.iter().map(|r| r.lines.len()).max().unwrap_or(0);
    let (mut natural, mut min) = (vec![0; columns], vec![0; columns]);
    for r in &rows {
        for (i, c) in r.lines.iter().enumerate() {
            let text = plain(c);
            let w = str_width(&text);
            let word = text.split_whitespace().map(str_width).max().unwrap_or(0).min(CELL_WORD_MAX).max(w.min(4));
            natural[i] = natural[i].max(w);
            min[i] = min[i].max(word);
        }
    }
    let gaps = 3 * columns.saturating_sub(1);
    let fits = |w: &[usize]| w.iter().sum::<usize>() + gaps <= width;
    let header = match rows.first() {
        Some(Block { kind: Kind::Row { header: true, .. }, lines, .. }) => Some(lines.iter().map(|c| one_line(&plain(c))).collect()),
        _ => None,
    };
    let (mode, widths) = if fits(&natural) {
        (Mode::Grid, natural)
    } else if fits(&min) {
        (Mode::Wrapped, share(&natural, min, width - gaps))
    } else {
        (Mode::Records, natural)
    };
    let ruled = mode == Mode::Wrapped && rows.iter().any(|r| matches!(r.kind, Kind::Row { header: false, .. }) && row_cells(r, &widths, Style::new()).iter().any(|c| c.len() > 1));
    Table { widths, header, mode, ruled }
}

/// Spare width goes to each column in proportion to its natural width, never past it.
fn share(natural: &[usize], mut widths: Vec<usize>, avail: usize) -> Vec<usize> {
    let mut spare = avail.saturating_sub(widths.iter().sum());
    while spare > 0 {
        let open: Vec<usize> = (0..widths.len()).filter(|&i| widths[i] < natural[i]).collect();
        let total: usize = open.iter().map(|&i| natural[i]).sum();
        if open.is_empty() || total == 0 {
            break;
        }
        let mut given = 0;
        for &i in &open {
            let add = (spare * natural[i] / total).min(natural[i] - widths[i]);
            widths[i] += add;
            given += add;
        }
        if given == 0 {
            for &i in open.iter().take(spare) {
                widths[i] += 1;
                given += 1;
            }
        }
        spare -= given;
    }
    widths
}

fn row_cells(b: &Block, widths: &[usize], base: Style) -> Vec<Vec<Line<'static>>> {
    b.lines.iter().zip(widths).map(|(c, &w)| if w == 0 { vec![Line::default()] } else { wrap(spans(c, base, false), w, vec![], vec![]) }).collect()
}

fn pad(line: Line<'static>, width: usize, align: Alignment) -> Vec<Span<'static>> {
    let gap = width.saturating_sub(line.width());
    let (before, after) = match align {
        Alignment::Right => (gap, 0),
        Alignment::Center => (gap / 2, gap - gap / 2),
        Alignment::Left | Alignment::None => (0, gap),
    };
    let mut out = vec![Span::raw(" ".repeat(before))];
    out.extend(line.spans);
    out.push(Span::raw(" ".repeat(after)));
    out
}

fn separator(widths: &[usize]) -> Line<'static> {
    Line::styled(widths.iter().map(|w| "─".repeat(*w)).collect::<Vec<_>>().join("─┼─"), Style::new().fg(DIM))
}

pub fn render(blocks: &[Block], width: usize) -> Rendered {
    let width = width.max(8);
    let mut lines: Vec<Line<'static>> = vec![];
    let mut ranges = vec![];
    let mut table = Table::default();
    for (i, b) in blocks.iter().enumerate() {
        let indent = if matches!(b.kind, Kind::Item(_)) { 0 } else { 2 * b.depth };
        let inner = width.saturating_sub(indent).max(8);
        let row = |k: &Kind| matches!(k, Kind::Row { header: false, .. });
        if i > 0 && !joins_previous(&blocks[i - 1].kind, &b.kind) {
            lines.push(Line::default());
        } else if i > 0 && table.ruled && row(&blocks[i - 1].kind) && row(&b.kind) {
            lines.push(indented(separator(&table.widths), indent));
        }
        if matches!(b.kind, Kind::Row { .. }) && (i == 0 || !matches!(blocks[i - 1].kind, Kind::Row { .. })) {
            table = table_at(blocks, i, inner);
        }
        let start = lines.len();
        lines.extend(render_block(b, inner, &table).into_iter().map(|l| indented(l, indent)));
        ranges.push((start, lines.len()));
    }
    Rendered { lines, ranges }
}

fn indented(line: Line<'static>, indent: usize) -> Line<'static> {
    if indent == 0 {
        return line;
    }
    let mut spans = vec![Span::raw(" ".repeat(indent))];
    spans.extend(line.spans);
    Line::from(spans).style(line.style)
}

/// A table too wide for the screen reads as one record per row, each cell under its column name.
fn record(cs: &[Text], header: Option<&[String]>, width: usize) -> Vec<Line<'static>> {
    let mut out = vec![];
    for (i, c) in cs.iter().enumerate() {
        let lead = if i == 0 { "▸ " } else { "  " };
        let mut first = vec![Span::styled(lead, Style::new().fg(ACCENT))];
        if let Some(name) = header.and_then(|h| h.get(i)).filter(|n| !n.trim().is_empty()) {
            first.push(Span::styled(format!("{name}: "), Style::new().fg(DIM).add_modifier(Modifier::BOLD)));
        }
        out.extend(wrap(spans(c, Style::new(), false), width, first, vec![Span::raw("    ")]));
    }
    out
}

fn wrap_text(b: &Block, base: Style, width: usize, first: Vec<Span<'static>>, rest: Vec<Span<'static>>) -> Vec<Line<'static>> {
    b.lines.iter().enumerate().flat_map(|(k, l)| wrap(spans(l, base, true), width, if k == 0 { first.clone() } else { rest.clone() }, rest.clone())).collect()
}

fn render_block(b: &Block, width: usize, table: &Table) -> Vec<Line<'static>> {
    match &b.kind {
        Kind::Heading(level) => {
            let style = match level {
                1 => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                2 => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                _ => Style::new().add_modifier(Modifier::BOLD),
            };
            wrap_text(b, style, width, vec![], vec![])
        }
        Kind::Para => wrap_text(b, Style::new(), width, vec![], vec![]),
        Kind::Item(marker) => {
            let indent = "  ".repeat(b.depth);
            let color = if marker.ends_with('.') { ACCENT } else { DIM };
            let first = vec![Span::raw(indent.clone()), Span::styled(format!("{marker} "), Style::new().fg(color))];
            let rest = vec![Span::raw(format!("{indent}{}", " ".repeat(str_width(marker) + 1)))];
            wrap_text(b, Style::new(), width, first, rest)
        }
        Kind::Quote => {
            let bar = || vec![Span::styled("▎ ", Style::new().fg(DIM))];
            wrap_text(b, Style::new().fg(Color::Gray).add_modifier(Modifier::ITALIC), width, bar(), bar())
        }
        Kind::Code(lang) => {
            let prefix = Span::styled("│ ", Style::new().fg(DIM));
            let mut out: Vec<Line<'static>> = lang.iter().map(|l| Line::styled(truncate(l, width), Style::new().fg(DIM))).collect();
            let lines = if b.lines.is_empty() { vec![String::new()] } else { b.lines.iter().map(|l| plain(l)).collect() };
            out.extend(lines.iter().flat_map(|l| hard_wrap(l, width, prefix.clone(), Style::new().fg(CODE))));
            out
        }
        Kind::Row { header, aligns } => {
            if table.mode == Mode::Records || table.widths.len() < b.lines.len() {
                return match (header, table.header.as_deref()) {
                    (true, Some(_)) => vec![],
                    (_, names) => record(&b.lines, names, width),
                };
            }
            let base = if *header { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
            let mut cells = row_cells(b, &table.widths, base);
            let height = cells.iter().map(Vec::len).max().unwrap_or(1);
            let mut out = vec![];
            for k in 0..height {
                let mut spans = vec![];
                for (i, w) in table.widths.iter().enumerate() {
                    if i > 0 {
                        spans.push(Span::styled(" │ ", Style::new().fg(DIM)));
                    }
                    let line = cells.get_mut(i).and_then(|c| c.get_mut(k)).map(std::mem::take).unwrap_or_default();
                    spans.extend(pad(line, *w, aligns.get(i).copied().unwrap_or(Alignment::None)));
                }
                out.push(Line::from(spans));
            }
            if *header {
                out.push(separator(&table.widths));
            }
            out
        }
        Kind::Rule => vec![Line::styled("─".repeat(width), Style::new().fg(DIM))],
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

    fn rendered(md: &str, width: usize) -> Vec<String> {
        render(&parse(md), width).lines.iter().map(|l| text_of(l).trim_end().to_string()).collect()
    }

    #[test]
    fn parses_the_blocks_a_plan_is_made_of() {
        let md = "# Plan\n\nFirst paragraph\ncontinues here.\n\n- one\n  continues one\n- [x] two\n  1. nested\n\n```rust\nfn main() {}\n\n```\n\n> long\n> quote\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n---\n";
        let b = parse(md);
        let kinds: Vec<(&Kind, usize)> = b.iter().map(|b| (&b.kind, b.depth)).collect();
        let row = |header| Kind::Row { header, aligns: vec![Alignment::None; 2] };
        assert_eq!(
            kinds,
            vec![
                (&Kind::Heading(1), 0),
                (&Kind::Para, 0),
                (&Kind::Item("•".into()), 0),
                (&Kind::Item("☑".into()), 0),
                (&Kind::Item("1.".into()), 1),
                (&Kind::Code(Some("rust".into())), 0),
                (&Kind::Quote, 0),
                (&row(true), 0),
                (&row(false), 0),
                (&Kind::Rule, 0),
            ]
        );
        assert_eq!(b[1].plain(), "First paragraph continues here.");
        assert_eq!(b[2].plain(), "one continues one");
        assert_eq!(b[5].lines.iter().map(|l| plain(l)).collect::<Vec<_>>(), vec!["fn main() {}".to_string(), String::new()]);
        assert_eq!(b[7].plain(), "a | b");
    }

    /// hive/aso-skills, 23 Sep: a three-column table with long cells showed as wrapped `a │ b │ c`.
    const WIDE: &str = "## Tabla\n\n| Lo que traen | ¿Lo tenemos? | ¿Cambia lo que haríamos? |\n|---|---|---|\n| **Fuente de datos = Appeeky** (API/MCP de pago, 8 $/mes) para volumen y rankings | No, y a propósito | **No.** Sin esa clave sus skills no tienen dato: recomiendan de memoria |\n| Fórmula de oportunidad | Parecido | **No.** Un número inventado ordena peor que un juicio explícito |\n";

    #[test]
    fn a_table_is_a_grid_then_a_wrapped_grid_then_one_record_per_row() {
        let wide = rendered(WIDE, 200);
        assert!(wide.iter().any(|l| l.starts_with("Lo que traen") && l.contains(" │ ¿Lo tenemos?")), "{wide:#?}");
        assert_eq!(wide.iter().filter(|l| l.contains("─┼─")).count(), 1, "{wide:#?}");

        let wrapped = rendered(WIDE, 60);
        assert!(wrapped.iter().all(|l| str_width(l) <= 60), "{wrapped:#?}");
        assert!(wrapped.iter().any(|l| l.starts_with("Fuente de datos") && l.contains(" │ No, y a ")), "{wrapped:#?}");
        assert_eq!(wrapped.iter().filter(|l| l.contains("─┼─")).count(), 2, "header rule and one rule between the two rows: {wrapped:#?}");

        let narrow = rendered(WIDE, 36);
        assert!(narrow.iter().all(|l| !l.contains(" │ ") && str_width(l) <= 36), "{narrow:#?}");
        let records: Vec<&String> = narrow.iter().filter(|l| l.starts_with("▸ ")).collect();
        assert_eq!(records.len(), 2, "{narrow:#?}");
        assert!(records[0].starts_with("▸ Lo que traen: Fuente de datos"), "{narrow:#?}");
        assert!(narrow.iter().any(|l| l.starts_with("  ¿Lo tenemos?: No, y a")), "{narrow:#?}");
        assert!(narrow.iter().any(|l| l.starts_with("  ¿Cambia lo que haríamos?: No.")), "{narrow:#?}");
    }

    #[test]
    fn a_wide_table_with_an_empty_header_still_reads_row_by_row() {
        let narrow = rendered("| | | |\n|---|---|---|\n| aaaaaaaaaaaa | bbbbbbbbbbbbbbbb | cccccccccccccc |\n", 20);
        assert_eq!(narrow.iter().filter(|l| l.starts_with("▸ ")).count(), 1, "{narrow:#?}");
        assert!(narrow.iter().all(|l| !l.contains(" │ ") && !l.contains(": ")), "{narrow:#?}");
    }

    #[test]
    fn cells_follow_the_alignment_of_the_separator_row() {
        let lines = rendered("| left | right | mid |\n|:--|--:|:-:|\n| a | b | c |\n", 40);
        assert_eq!(lines, vec!["left │ right │ mid", "─────┼───────┼────", "a    │     b │  c"]);
    }

    #[test]
    fn inline_styles_become_span_styles() {
        let line = &render(&parse("**b** *i* ***bi*** ~~s~~ `c` [t](https://docs.rs/x)"), 80).lines[0];
        let style_of = |text: &str| line.spans.iter().find(|s| s.content == text).unwrap_or_else(|| panic!("{text} in {line:?}")).style;
        let cases = [
            ("b", Modifier::BOLD),
            ("i", Modifier::ITALIC),
            ("bi", Modifier::BOLD | Modifier::ITALIC),
            ("s", Modifier::CROSSED_OUT),
            ("t", Modifier::UNDERLINED),
        ];
        for (text, m) in cases {
            assert!(style_of(text).add_modifier.contains(m), "{text}: {:?}", style_of(text));
        }
        assert_eq!(style_of("c").fg, Some(CODE));
        assert_eq!(style_of("(docs.rs)").fg, Some(DIM));
    }

    #[test]
    fn a_link_shows_its_host_unless_the_text_already_is_the_url() {
        let cases = [
            ("[the guide](https://www.docs.rs/a?b)", "the guide (docs.rs)"),
            ("<https://example.com>", "https://example.com"),
            ("[https://x.y](https://x.y)", "https://x.y"),
            ("[x.y](https://x.y/z)", "x.y"),
            ("[notes](notes.md)", "notes"),
            ("| a |\n|---|\n| [cell](https://x.y) |", "cell"),
        ];
        for (md, shown) in cases {
            let lines = rendered(md, 80);
            assert_eq!(lines.last().map(String::as_str), Some(shown), "{md}");
        }
    }

    #[test]
    fn a_hard_break_starts_a_new_line_under_the_same_prefix() {
        assert_eq!(rendered("- one  \n  two\\\n  three", 40), vec!["• one", "  two", "  three"]);
        assert_eq!(parse("one  \ntwo")[0].quote(), "one two");
    }

    #[test]
    fn code_shows_its_language_above_it() {
        assert_eq!(rendered("```bash\nls\n```", 40), vec!["bash", "│ ls"]);
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
        assert_eq!(parse("my_var_x and 2 * 3 * 4")[0].plain(), "my_var_x and 2 * 3 * 4");
        assert_eq!(parse(r"52\. two \*not\*")[0].plain(), "52. two *not*");
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
