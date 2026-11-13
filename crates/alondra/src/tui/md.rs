use pulldown_cmark::{Alignment, CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub(crate) mod diff;
pub(crate) use alondra_core::markdown::html;
mod mermaid;

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
    Row {
        header: bool,
        aligns: Vec<Alignment>,
    },
    Rule,
    Label,
    /// A diff's totals, over the list of its files.
    DiffFiles { truncated: bool },
    DiffEntry(diff::Header),
    DiffFile(diff::Header),
    DiffLine(diff::Row),
    /// An image the terminal draws under its caption, `cols`×`rows` cells at its own size.
    Image {
        cols: u16,
        rows: u16,
    },
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Fmt {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    link: bool,
    host: bool,
    /// Stands in for an image (its glyph, or its file name when it has no alt text): shown, never quoted.
    mark: bool,
    /// The block's target this text opens.
    href: Option<u8>,
    /// What changed inside a diff line.
    changed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TargetKind {
    Link,
    Image,
    /// A file Alondra stores, by its content route.
    Stored,
}

/// Something a block lets the user open: a link, an image, or the file it stands for.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub kind: TargetKind,
    pub url: String,
    pub label: String,
}

impl Target {
    /// Only the web and mail open from a document: anything else would run whatever an agent wrote.
    pub fn openable(&self) -> bool {
        self.stored().is_some() || (web(&self.url) && !self.url.chars().any(char::is_control))
    }

    pub fn local(&self) -> bool {
        self.stored().is_none() && !self.url.contains(':')
    }

    /// The daemon's route to the file this opens: an attachment, or an image the document's
    /// author had copied into Alondra's files folder.
    pub fn stored(&self) -> Option<&str> {
        match self.kind {
            TargetKind::Stored => Some(&self.url),
            TargetKind::Image => crate::doc_images::stored_image(&self.url).map(|_| self.url.as_str()),
            TargetKind::Link => None,
        }
    }
}

fn web(url: &str) -> bool {
    let scheme = url.split_once(':').map(|(s, _)| s.to_ascii_lowercase());
    matches!(scheme.as_deref(), Some("http" | "https" | "mailto"))
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
    pub targets: Vec<Target>,
}

impl Block {
    fn new(kind: Kind, depth: usize) -> Self {
        let lines = if matches!(kind, Kind::Row { .. } | Kind::Rule) {
            vec![]
        } else {
            vec![vec![]]
        };
        Block {
            kind,
            depth,
            lines,
            targets: vec![],
        }
    }

    fn plain_text(kind: Kind, text: &str) -> Self {
        Block {
            kind,
            depth: 0,
            lines: vec![vec![Run {
                text: text.to_string(),
                fmt: Fmt::default(),
            }]],
            targets: vec![],
        }
    }

    pub fn opening(mut self, target: Target) -> Self {
        self.targets.push(target);
        self
    }

    pub fn label(text: &str) -> Self {
        Block::plain_text(Kind::Label, text)
    }

    pub fn para(text: &str) -> Self {
        Block::plain_text(Kind::Para, text)
    }

    pub fn image(caption: &str, cols: u16, rows: u16) -> Self {
        Block::plain_text(Kind::Image { cols, rows }, caption)
    }

    pub fn plain(&self) -> String {
        let (sep, lines): (&str, Vec<String>) = match self.kind {
            Kind::Row { .. } => (
                " | ",
                self.lines.iter().map(|c| one_line(&plain(c))).collect(),
            ),
            Kind::Code(_) => (
                " ",
                self.lines.iter().map(|l| one_line(&plain(l))).collect(),
            ),
            _ => (
                "\n",
                self.lines.iter().map(|l| one_line(&plain(l))).collect(),
            ),
        };
        lines.join(sep).trim().to_string()
    }

    pub fn quote(&self) -> String {
        match &self.kind {
            Kind::DiffLine(row) => clip(&format!("{} {}", row.place(), one_line(&self.plain())), QUOTE_MAX),
            _ => clip(&one_line(&self.plain()), QUOTE_MAX),
        }
    }

    pub fn commentable(&self) -> bool {
        match &self.kind {
            Kind::Rule | Kind::Label | Kind::DiffFiles { .. } | Kind::DiffEntry(_) | Kind::DiffFile(_) => false,
            Kind::DiffLine(row) => row.commentable(),
            _ => true,
        }
    }

    /// Where the reader's cursor stops: what takes a comment, and a diff's files, which open and fold.
    pub fn selectable(&self) -> bool {
        self.commentable() || matches!(self.kind, Kind::DiffEntry(_) | Kind::DiffFile(_))
    }
}

fn plain(text: &[Run]) -> String {
    text.iter()
        .filter(|r| !r.fmt.host && !r.fmt.mark)
        .map(|r| r.text.as_str())
        .collect()
}

pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!(
            "{}…",
            s.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
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
    links: Vec<Open>,
    code: Option<String>,
    /// An HTML tag or comment the last HTML line left open.
    html_rest: String,
}

/// A link or image being read: the destination whose host it shows, where its text starts on the
/// line, and the block target it opens.
struct Open {
    host_of: String,
    at: usize,
    target: Option<u8>,
    image: bool,
    html: bool,
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
        self.cur
            .as_ref()
            .and_then(|b| b.lines.last())
            .map(|l| plain(l))
            .unwrap_or_default()
    }

    fn push(&mut self, text: &str, host: bool) {
        self.push_fmt(
            text,
            Fmt {
                host,
                ..Fmt::default()
            },
        );
    }

    fn push_fmt(&mut self, text: &str, extra: Fmt) {
        if self.cur.is_none() {
            self.open_text();
        }
        let href = self
            .links
            .iter()
            .rev()
            .find(|o| !o.image && o.target.is_some())
            .or_else(|| self.links.iter().rev().find(|o| o.target.is_some()))
            .and_then(|o| o.target);
        let fmt = Fmt {
            bold: self.strong > 0,
            italic: self.emphasis > 0,
            strike: self.strike > 0,
            link: href.is_some() && !extra.host,
            href: href.filter(|_| !extra.host),
            ..extra
        };
        let Some(b) = self.cur.as_mut() else { return };
        if b.lines.is_empty() {
            b.lines.push(vec![]);
        }
        let line = b.lines.last_mut().expect("a line was just ensured");
        match line.last_mut() {
            Some(r) if r.fmt == fmt => r.text.push_str(text),
            _ => line.push(Run {
                text: text.to_string(),
                fmt,
            }),
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
            Event::Code(t) => self.push_fmt(
                &t,
                Fmt {
                    code: true,
                    ..Fmt::default()
                },
            ),
            Event::InlineHtml(t) | Event::Html(t) => self.html(&t),
            Event::InlineMath(t) | Event::DisplayMath(t) | Event::FootnoteReference(t) => {
                self.push(&t, false)
            }
            Event::SoftBreak => self.push(" ", false),
            Event::HardBreak => self.hard_break(),
            Event::Rule => {
                self.open(Kind::Rule, self.nesting());
                self.flush();
            }
            Event::TaskListMarker(done) => {
                if let Some(Block {
                    kind: Kind::Item(marker),
                    ..
                }) = self.cur.as_mut()
                {
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
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().map(str::to_string)
                    }
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
            Tag::TableHead | Tag::TableRow => self.open(
                Kind::Row {
                    header: tag == Tag::TableHead,
                    aligns: self.aligns.clone(),
                },
                self.nesting(),
            ),
            Tag::TableCell => {
                if let Some(b) = self.cur.as_mut() {
                    b.lines.push(vec![]);
                }
            }
            Tag::Emphasis => self.emphasis += 1,
            Tag::Strong => self.strong += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link {
                link_type,
                dest_url,
                ..
            } => {
                let url = match link_type {
                    LinkType::Email if !dest_url.starts_with("mailto:") => {
                        format!("mailto:{dest_url}")
                    }
                    _ => dest_url.to_string(),
                };
                let shown = matches!(link_type, LinkType::Autolink | LinkType::Email);
                self.open_link(TargetKind::Link, url, !shown, false);
            }
            Tag::Image { dest_url, .. } => {
                self.open_link(TargetKind::Image, dest_url.to_string(), true, false)
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::HtmlBlock => {
                let rest = std::mem::take(&mut self.html_rest);
                if !rest.is_empty() {
                    self.push(&rest, false);
                }
                self.close_html_block();
            }
            TagEnd::Paragraph
            | TagEnd::Heading(_)
            | TagEnd::Item
            | TagEnd::TableHead
            | TagEnd::TableRow => self.flush(),
            TagEnd::CodeBlock => {
                let code = self.code.take().unwrap_or_default();
                let code = code.strip_suffix('\n').unwrap_or(&code);
                if let Some(b) = self.cur.as_mut() {
                    let fmt = Fmt {
                        code: true,
                        ..Fmt::default()
                    };
                    b.lines = code
                        .split('\n')
                        .map(|l| {
                            vec![Run {
                                text: l.replace('\t', "    "),
                                fmt,
                            }]
                        })
                        .collect();
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
            TagEnd::Link | TagEnd::Image => self.close_link(),
            _ => {}
        }
    }

    fn hard_break(&mut self) {
        match self.cur.as_mut() {
            Some(b) if !matches!(b.kind, Kind::Row { .. }) => b.lines.push(vec![]),
            _ => self.push(" ", false),
        }
    }

    fn target(&mut self, kind: TargetKind, url: String) -> Option<u8> {
        if self.cur.is_none() {
            self.open_text();
        }
        let targets = &mut self.cur.as_mut()?.targets;
        let at = match targets.iter().position(|t| t.kind == kind && t.url == url) {
            Some(at) => at,
            None => {
                targets.push(Target {
                    kind,
                    url,
                    label: String::new(),
                });
                targets.len() - 1
            }
        };
        u8::try_from(at).ok()
    }

    fn open_link(&mut self, kind: TargetKind, url: String, shows_host: bool, html: bool) {
        let host_of = if shows_host { url.clone() } else { String::new() };
        let target = self.target(kind, url);
        let image = kind == TargetKind::Image;
        self.links.push(Open {
            host_of,
            at: 0,
            target,
            image,
            html,
        });
        if image {
            self.push_fmt(
                "📷 ",
                Fmt {
                    mark: true,
                    ..Fmt::default()
                },
            );
        }
        let at = self.current_line().len();
        if let Some(open) = self.links.last_mut() {
            open.at = at;
        }
    }

    fn close_link(&mut self) {
        let Some(open) = self.links.last() else {
            return;
        };
        let line = self.current_line();
        let mut text = line.get(open.at..).unwrap_or_default().trim().to_string();
        let (image, target, dest) = (open.image, open.target, open.host_of.clone());
        let url = target
            .and_then(|t| self.cur.as_ref()?.targets.get(t as usize))
            .map(|t| t.url.clone())
            .unwrap_or_default();
        if image && text.is_empty() {
            text = match crate::doc_images::stored_image(&url) {
                Some(_) => tr!("image", "imagen").to_string(),
                None => file_name(&url).to_string(),
            };
            self.push_fmt(
                &text,
                Fmt {
                    mark: true,
                    ..Fmt::default()
                },
            );
        }
        self.links.pop();
        if let Some(t) = target.and_then(|t| self.cur.as_mut()?.targets.get_mut(t as usize)) {
            if t.label.is_empty() {
                t.label = if text.is_empty() { url.clone() } else { text.clone() };
            }
        }
        let local = !url.is_empty() && !url.contains(':') && crate::doc_images::stored_image(&url).is_none();
        if image && self.links.iter().any(|o| !o.image && o.target.is_some()) {
            return;
        }
        if image && local {
            self.push(
                tr!(" (local image, not available here)", " (imagen local, no disponible aquí)"),
                true,
            );
        } else if let Some(h) = host(&dest).filter(|h| text != dest && text != *h) {
            self.push(&format!(" ({h})"), true);
        }
    }

    fn html(&mut self, t: &str) {
        let mut src = std::mem::take(&mut self.html_rest);
        src.push_str(t);
        let (pieces, rest) = html::pieces(&src);
        for piece in pieces {
            match piece {
                html::Piece::Text(text) => {
                    let text = collapse(&text);
                    let text = if self.line_ends_in_space() { text.trim_start() } else { &text };
                    if !text.is_empty() {
                        self.push(text, false);
                    }
                }
                html::Piece::Comment => {}
                html::Piece::Tag(tag) => self.html_tag(&tag),
            }
        }
        self.html_rest = rest.to_string();
    }

    fn html_tag(&mut self, tag: &html::Tag) {
        let closing = tag.closing;
        match tag.name.as_str() {
            "img" if !closing => {
                if let Some(src) = tag.attr("src").filter(|s| !s.is_empty()) {
                    self.open_link(TargetKind::Image, src.to_string(), true, true);
                    if let Some(alt) = tag.attr("alt") {
                        self.push(alt, false);
                    }
                    self.close_link();
                }
            }
            "img" | "source" => {}
            "a" if closing => {
                if self.links.last().is_some_and(|o| o.html) {
                    self.close_link();
                }
            }
            "a" => match tag.attr("href").filter(|h| !h.is_empty()) {
                Some(href) => self.open_link(TargetKind::Link, href.to_string(), true, true),
                None => self.links.push(Open {
                    host_of: String::new(),
                    at: 0,
                    target: None,
                    image: false,
                    html: true,
                }),
            },
            "b" | "strong" => {
                self.strong = if closing { self.strong.saturating_sub(1) } else { self.strong + 1 }
            }
            "i" | "em" => {
                self.emphasis = if closing {
                    self.emphasis.saturating_sub(1)
                } else {
                    self.emphasis + 1
                }
            }
            "br" => self.hard_break(),
            "p" | "div" | "center" | "details" | "summary" | "h1" | "h2" | "h3" | "h4" | "h5"
            | "h6" => {
                if !self.line_blank() {
                    self.hard_break();
                }
            }
            "span" | "picture" | "sub" | "sup" => {}
            _ => self.push(tag.raw, false),
        }
    }

    fn line_ends_in_space(&self) -> bool {
        self.cur
            .as_ref()
            .and_then(|b| b.lines.last())
            .is_none_or(|l| {
                l.iter()
                    .rev()
                    .find(|r| !r.text.is_empty())
                    .is_none_or(|r| r.text.ends_with(char::is_whitespace))
            })
    }

    fn line_blank(&self) -> bool {
        self.cur
            .as_ref()
            .and_then(|b| b.lines.last())
            .is_none_or(|l| blank(l))
    }

    /// An HTML block that only held layout (`<p align=center>`, `<br>`) leaves nothing behind.
    fn close_html_block(&mut self) {
        if let Some(b) = self.cur.as_mut() {
            while b.lines.len() > 1 && b.lines.last().is_some_and(|l| blank(l)) {
                b.lines.pop();
            }
            if b.targets.is_empty() && b.lines.iter().all(|l| blank(l)) {
                self.cur = None;
            }
        }
        self.flush();
    }
}

/// HTML text the way a browser lays it out: every run of spaces and line breaks is one space.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_whitespace() {
            if !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn blank(line: &[Run]) -> bool {
    line.iter().all(|r| r.text.trim().is_empty())
}

fn file_name(url: &str) -> &str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty())
        .unwrap_or(path)
}

pub fn parse(text: &str) -> Vec<Block> {
    let mut b = Builder::default();
    for event in Parser::new_ext(
        text,
        Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH,
    ) {
        b.event(event);
    }
    b.flush();
    b.out
}

fn style(fmt: Fmt, base: Style) -> Style {
    let mut s = base;
    for (on, m) in [
        (fmt.bold, Modifier::BOLD),
        (fmt.italic, Modifier::ITALIC),
        (fmt.strike, Modifier::CROSSED_OUT),
    ] {
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
    if let Some(t) = fmt.href {
        s = s.underline_color(Color::Rgb(0, 0, t));
    }
    if fmt.host {
        s = s.fg(DIM);
    }
    s
}

fn spans(text: &[Run], base: Style, hosts: bool) -> Vec<Span<'static>> {
    text.iter()
        .filter(|r| hosts || !r.fmt.host)
        .map(|r| Span::styled(r.text.clone(), style(r.fmt, base)))
        .collect()
}

/// Markdown for a field that stays on one line: bold, italic, code and links keep their style, and
/// headings, paragraphs and list items join with « · », without their marks.
pub fn inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    for block in parse(text).iter().filter(|block| block.kind != Kind::Rule) {
        let mut joined: Vec<Span<'static>> = Vec::new();
        for line in &block.lines {
            let line = spans(line, base, false);
            if line.iter().all(|span| span.content.trim().is_empty()) {
                continue;
            }
            if !joined.is_empty() {
                joined.push(Span::styled(" ", base));
            }
            joined.extend(line);
        }
        if joined.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(Span::styled(" · ", base));
        }
        out.extend(joined);
    }
    out
}

/// The words of `inline`, for a place that draws no styles: a key bar, a menu title.
pub fn inline_text(text: &str) -> String {
    inline(text, Style::new()).iter().map(|span| span.content.as_ref()).collect()
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
    while line
        .last()
        .is_some_and(|s| !s.content.is_empty() && s.content.trim().is_empty())
    {
        line.pop();
    }
    Line::from(line)
}

pub fn wrap(
    spans: Vec<Span<'static>>,
    width: usize,
    first: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
) -> Vec<Line<'static>> {
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
            out.push(Line::from(vec![
                prefix.clone(),
                Span::styled(std::mem::take(&mut cur), style),
            ]));
            w = 0;
        }
        cur.push(ch);
        w += cw;
    }
    out.push(Line::from(vec![prefix, Span::styled(cur, style)]));
    out
}

pub const IMAGE_MAX_ROWS: usize = 20;

/// The rows an image takes at `width` cells: its own size, smaller when it is wider than the
/// text, and never taller than `IMAGE_MAX_ROWS`.
pub fn image_rows(cols: u16, rows: u16, width: usize) -> usize {
    let (cols, rows) = (usize::from(cols.max(1)), usize::from(rows));
    let rows = if cols > width { (rows * width).div_ceil(cols) } else { rows };
    rows.clamp(1, IMAGE_MAX_ROWS)
}

pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub ranges: Vec<(usize, usize)>,
}

fn joins_previous(prev: &Kind, cur: &Kind) -> bool {
    diff::joins(prev, cur)
        || matches!(
            (prev, cur),
            (Kind::Item(_), Kind::Item(_)) | (Kind::Row { .. }, Kind::Row { .. })
        )
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
    let rows: Vec<&Block> = blocks[start..]
        .iter()
        .take_while(|b| matches!(b.kind, Kind::Row { .. }))
        .collect();
    let columns = rows.iter().map(|r| r.lines.len()).max().unwrap_or(0);
    let (mut natural, mut min) = (vec![0; columns], vec![0; columns]);
    for r in &rows {
        for (i, c) in r.lines.iter().enumerate() {
            let text = plain(c);
            let w = str_width(&text);
            let word = text
                .split_whitespace()
                .map(str_width)
                .max()
                .unwrap_or(0)
                .min(CELL_WORD_MAX)
                .max(w.min(4));
            natural[i] = natural[i].max(w);
            min[i] = min[i].max(word);
        }
    }
    let gaps = 3 * columns.saturating_sub(1);
    let fits = |w: &[usize]| w.iter().sum::<usize>() + gaps <= width;
    let header = match rows.first() {
        Some(Block {
            kind: Kind::Row { header: true, .. },
            lines,
            ..
        }) => Some(lines.iter().map(|c| one_line(&plain(c))).collect()),
        _ => None,
    };
    let (mode, widths) = if fits(&natural) {
        (Mode::Grid, natural)
    } else if fits(&min) {
        (Mode::Wrapped, share(&natural, min, width - gaps))
    } else {
        (Mode::Records, natural)
    };
    let ruled = mode == Mode::Wrapped
        && rows.iter().any(|r| {
            matches!(r.kind, Kind::Row { header: false, .. })
                && row_cells(r, &widths, Style::new())
                    .iter()
                    .any(|c| c.len() > 1)
        });
    Table {
        widths,
        header,
        mode,
        ruled,
    }
}

/// Spare width goes to each column in proportion to its natural width, never past it.
fn share(natural: &[usize], mut widths: Vec<usize>, avail: usize) -> Vec<usize> {
    let mut spare = avail.saturating_sub(widths.iter().sum());
    while spare > 0 {
        let open: Vec<usize> = (0..widths.len())
            .filter(|&i| widths[i] < natural[i])
            .collect();
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
    b.lines
        .iter()
        .zip(widths)
        .map(|(c, &w)| {
            if w == 0 {
                vec![Line::default()]
            } else {
                wrap(spans(c, base, false), w, vec![], vec![])
            }
        })
        .collect()
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
    Line::styled(
        widths
            .iter()
            .map(|w| "─".repeat(*w))
            .collect::<Vec<_>>()
            .join("─┼─"),
        Style::new().fg(DIM),
    )
}

pub fn render(blocks: &[Block], width: usize) -> Rendered {
    let mut rendered = render_linked(blocks, width);
    for span in rendered.lines.iter_mut().flat_map(|l| l.spans.iter_mut()) {
        span.style.underline_color = None;
    }
    rendered
}

/// Like `render`, with each piece of text that opens a target tagged in its underline colour,
/// for `hyperlinks` to find on screen.
pub fn render_linked(blocks: &[Block], width: usize) -> Rendered {
    let width = width.max(8);
    let mut lines: Vec<Line<'static>> = vec![];
    let mut ranges = vec![];
    let mut table = Table::default();
    let mut shown: Option<usize> = None;
    for (i, b) in blocks.iter().enumerate() {
        if diff::hidden(&b.kind) {
            ranges.push((lines.len(), lines.len()));
            continue;
        }
        let previous = shown.replace(i);
        let indent = if matches!(b.kind, Kind::Item(_)) {
            0
        } else {
            2 * b.depth
        };
        let inner = width.saturating_sub(indent).max(8);
        let row = |k: &Kind| matches!(k, Kind::Row { header: false, .. });
        if previous.is_some_and(|p| !joins_previous(&blocks[p].kind, &b.kind)) {
            lines.push(Line::default());
        } else if i > 0 && table.ruled && row(&blocks[i - 1].kind) && row(&b.kind) {
            lines.push(indented(separator(&table.widths), indent));
        }
        if matches!(b.kind, Kind::Row { .. })
            && (i == 0 || !matches!(blocks[i - 1].kind, Kind::Row { .. }))
        {
            table = table_at(blocks, i, inner);
        }
        let start = lines.len();
        lines.extend(
            render_block(b, inner, &table)
                .into_iter()
                .map(|l| indented(tagged(l, i), indent)),
        );
        ranges.push((start, lines.len()));
    }
    Rendered { lines, ranges }
}

fn tagged(mut line: Line<'static>, block: usize) -> Line<'static> {
    for span in &mut line.spans {
        if let Some(Color::Rgb(0, 0, t)) = span.style.underline_color {
            span.style.underline_color = u16::try_from(block)
                .ok()
                .map(|b| Color::Rgb((b >> 8) as u8, b as u8, t));
        }
    }
    line
}

/// Turns the tagged text `render_linked` drew into terminal hyperlinks (OSC 8) to the target
/// each piece opens, so a terminal that knows them opens it on a click; with `emit` off it only
/// removes the tags.
pub fn hyperlinks(buf: &mut Buffer, area: Rect, blocks: &[Block], emit: bool) {
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        let mut x = area.left();
        while x < area.right() {
            let cell = &mut buf[(x, y)];
            let mut step = 1;
            if let Color::Rgb(hi, lo, t) = cell.underline_color {
                cell.underline_color = Color::Reset;
                let url = blocks
                    .get(usize::from(hi) << 8 | usize::from(lo))
                    .and_then(|b| b.targets.get(usize::from(t)))
                    .filter(|t| emit && t.stored().is_none() && t.openable())
                    .map(|t| t.url.clone());
                let symbol = cell.symbol().to_string();
                let width = str_width(&symbol).max(1);
                if let (Some(url), Some(forced)) = (url, std::num::NonZeroU16::new(width as u16)) {
                    cell.set_symbol(&format!("\x1b]8;;{url}\x1b\\{symbol}\x1b]8;;\x1b\\"));
                    cell.set_diff_option(CellDiffOption::ForcedWidth(forced));
                }
                step = width as u16;
            }
            for k in 1..step {
                if x + k < area.right() {
                    let hidden = &mut buf[(x + k, y)];
                    if matches!(hidden.underline_color, Color::Rgb(..)) {
                        hidden.underline_color = Color::Reset;
                    }
                }
            }
            x += step;
        }
    }
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
        if let Some(name) = header
            .and_then(|h| h.get(i))
            .filter(|n| !n.trim().is_empty())
        {
            first.push(Span::styled(
                format!("{name}: "),
                Style::new().fg(DIM).add_modifier(Modifier::BOLD),
            ));
        }
        out.extend(wrap(
            spans(c, Style::new(), false),
            width,
            first,
            vec![Span::raw("    ")],
        ));
    }
    out
}

fn wrap_text(
    b: &Block,
    base: Style,
    width: usize,
    first: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
) -> Vec<Line<'static>> {
    b.lines
        .iter()
        .enumerate()
        .flat_map(|(k, l)| {
            wrap(
                spans(l, base, true),
                width,
                if k == 0 { first.clone() } else { rest.clone() },
                rest.clone(),
            )
        })
        .collect()
}

fn render_block(b: &Block, width: usize, table: &Table) -> Vec<Line<'static>> {
    match &b.kind {
        Kind::Heading(level) => {
            let style = match level {
                1 => Style::new()
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                2 => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                _ => Style::new().add_modifier(Modifier::BOLD),
            };
            wrap_text(b, style, width, vec![], vec![])
        }
        Kind::Para => wrap_text(b, Style::new(), width, vec![], vec![]),
        Kind::Item(marker) => {
            let indent = "  ".repeat(b.depth);
            let color = if marker.ends_with('.') { ACCENT } else { DIM };
            let first = vec![
                Span::raw(indent.clone()),
                Span::styled(format!("{marker} "), Style::new().fg(color)),
            ];
            let rest = vec![Span::raw(format!(
                "{indent}{}",
                " ".repeat(str_width(marker) + 1)
            ))];
            wrap_text(b, Style::new(), width, first, rest)
        }
        Kind::Quote => {
            let bar = || vec![Span::styled("▎ ", Style::new().fg(DIM))];
            wrap_text(
                b,
                Style::new().fg(Color::Gray).add_modifier(Modifier::ITALIC),
                width,
                bar(),
                bar(),
            )
        }
        Kind::Code(lang) => {
            let prefix = Span::styled("│ ", Style::new().fg(DIM));
            let mut out: Vec<Line<'static>> = lang
                .iter()
                .map(|l| Line::styled(truncate(l, width), Style::new().fg(DIM)))
                .collect();
            let lines = if b.lines.is_empty() {
                vec![String::new()]
            } else {
                b.lines.iter().map(|l| plain(l)).collect()
            };
            if lang.as_deref().is_some_and(|l| l.eq_ignore_ascii_case("mermaid")) {
                match mermaid::draw(&lines.join("\n"), width.saturating_sub(2)) {
                    Ok(diagram) => {
                        out.extend(diagram.into_iter().map(|l| {
                            Line::from(vec![prefix.clone(), Span::styled(l, Style::new().fg(CODE))])
                        }));
                        return out;
                    }
                    Err(refusal) => out.extend(wrap(
                        vec![Span::styled(
                            refusal.notice(),
                            Style::new().fg(DIM).add_modifier(Modifier::ITALIC),
                        )],
                        width,
                        vec![],
                        vec![],
                    )),
                }
            }
            out.extend(
                lines
                    .iter()
                    .flat_map(|l| hard_wrap(l, width, prefix.clone(), Style::new().fg(CODE))),
            );
            out
        }
        Kind::Row { header, aligns } => {
            if table.mode == Mode::Records || table.widths.len() < b.lines.len() {
                return match (header, table.header.as_deref()) {
                    (true, Some(_)) => vec![],
                    (_, names) => record(&b.lines, names, width),
                };
            }
            let base = if *header {
                Style::new().add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            let mut cells = row_cells(b, &table.widths, base);
            let height = cells.iter().map(Vec::len).max().unwrap_or(1);
            let mut out = vec![];
            for k in 0..height {
                let mut spans = vec![];
                for (i, w) in table.widths.iter().enumerate() {
                    if i > 0 {
                        spans.push(Span::styled(" │ ", Style::new().fg(DIM)));
                    }
                    let line = cells
                        .get_mut(i)
                        .and_then(|c| c.get_mut(k))
                        .map(std::mem::take)
                        .unwrap_or_default();
                    spans.extend(pad(
                        line,
                        *w,
                        aligns.get(i).copied().unwrap_or(Alignment::None),
                    ));
                }
                out.push(Line::from(spans));
            }
            if *header {
                out.push(separator(&table.widths));
            }
            out
        }
        Kind::Rule => vec![Line::styled("─".repeat(width), Style::new().fg(DIM))],
        Kind::DiffFiles { .. } | Kind::DiffEntry(_) | Kind::DiffFile(_) | Kind::DiffLine(_) => diff::render(b, width),
        Kind::Image { cols, rows } => {
            let mut out = wrap_text(b, Style::new().fg(DIM), width, vec![], vec![]);
            out.extend(std::iter::repeat_n(Line::default(), image_rows(*cols, *rows, width)));
            out
        }
        Kind::Label => {
            let text = b.plain();
            let head = format!("── {} ", truncate(&text, width.saturating_sub(4)));
            let fill = width.saturating_sub(str_width(&head));
            vec![Line::styled(
                format!("{head}{}", "─".repeat(fill)),
                Style::new().fg(Color::Cyan),
            )]
        }
    }
}

pub fn feedback(comments: &[(String, String)], general: &str, author: &str) -> String {
    let mut out = format!("Comments from {author}:");
    for (i, (quote, text)) in comments.iter().enumerate() {
        let body = text
            .trim()
            .lines()
            .map(|l| format!("   {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        out.push_str(&format!(
            "\n\n{}. On \"{}\":\n{body}",
            i + 1,
            clip(quote, QUOTE_MAX)
        ));
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
        render(&parse(md), width)
            .lines
            .iter()
            .map(|l| text_of(l).trim_end().to_string())
            .collect()
    }

    #[test]
    fn parses_the_blocks_a_plan_is_made_of() {
        let md = "# Plan\n\nFirst paragraph\ncontinues here.\n\n- one\n  continues one\n- [x] two\n  1. nested\n\n```rust\nfn main() {}\n\n```\n\n> long\n> quote\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n---\n";
        let b = parse(md);
        let kinds: Vec<(&Kind, usize)> = b.iter().map(|b| (&b.kind, b.depth)).collect();
        let row = |header| Kind::Row {
            header,
            aligns: vec![Alignment::None; 2],
        };
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
        assert_eq!(
            b[5].lines.iter().map(|l| plain(l)).collect::<Vec<_>>(),
            vec!["fn main() {}".to_string(), String::new()]
        );
        assert_eq!(b[7].plain(), "a | b");
    }

    /// hive/aso-skills, 23 Sep: a three-column table with long cells showed as wrapped `a │ b │ c`.
    const WIDE: &str = "## Tabla\n\n| Lo que traen | ¿Lo tenemos? | ¿Cambia lo que haríamos? |\n|---|---|---|\n| **Fuente de datos = Appeeky** (API/MCP de pago, 8 $/mes) para volumen y rankings | No, y a propósito | **No.** Sin esa clave sus skills no tienen dato: recomiendan de memoria |\n| Fórmula de oportunidad | Parecido | **No.** Un número inventado ordena peor que un juicio explícito |\n";

    #[test]
    fn a_table_is_a_grid_then_a_wrapped_grid_then_one_record_per_row() {
        let wide = rendered(WIDE, 200);
        assert!(
            wide.iter()
                .any(|l| l.starts_with("Lo que traen") && l.contains(" │ ¿Lo tenemos?")),
            "{wide:#?}"
        );
        assert_eq!(
            wide.iter().filter(|l| l.contains("─┼─")).count(),
            1,
            "{wide:#?}"
        );

        let wrapped = rendered(WIDE, 60);
        assert!(wrapped.iter().all(|l| str_width(l) <= 60), "{wrapped:#?}");
        assert!(
            wrapped
                .iter()
                .any(|l| l.starts_with("Fuente de datos") && l.contains(" │ No, y a ")),
            "{wrapped:#?}"
        );
        assert_eq!(
            wrapped.iter().filter(|l| l.contains("─┼─")).count(),
            2,
            "header rule and one rule between the two rows: {wrapped:#?}"
        );

        let narrow = rendered(WIDE, 36);
        assert!(
            narrow
                .iter()
                .all(|l| !l.contains(" │ ") && str_width(l) <= 36),
            "{narrow:#?}"
        );
        let records: Vec<&String> = narrow.iter().filter(|l| l.starts_with("▸ ")).collect();
        assert_eq!(records.len(), 2, "{narrow:#?}");
        assert!(
            records[0].starts_with("▸ Lo que traen: Fuente de datos"),
            "{narrow:#?}"
        );
        assert!(
            narrow
                .iter()
                .any(|l| l.starts_with("  ¿Lo tenemos?: No, y a")),
            "{narrow:#?}"
        );
        assert!(
            narrow
                .iter()
                .any(|l| l.starts_with("  ¿Cambia lo que haríamos?: No.")),
            "{narrow:#?}"
        );
    }

    #[test]
    fn a_wide_table_with_an_empty_header_still_reads_row_by_row() {
        let narrow = rendered(
            "| | | |\n|---|---|---|\n| aaaaaaaaaaaa | bbbbbbbbbbbbbbbb | cccccccccccccc |\n",
            20,
        );
        assert_eq!(
            narrow.iter().filter(|l| l.starts_with("▸ ")).count(),
            1,
            "{narrow:#?}"
        );
        assert!(
            narrow
                .iter()
                .all(|l| !l.contains(" │ ") && !l.contains(": ")),
            "{narrow:#?}"
        );
    }

    #[test]
    fn cells_follow_the_alignment_of_the_separator_row() {
        let lines = rendered("| left | right | mid |\n|:--|--:|:-:|\n| a | b | c |\n", 40);
        assert_eq!(
            lines,
            vec![
                "left │ right │ mid",
                "─────┼───────┼────",
                "a    │     b │  c"
            ]
        );
    }

    #[test]
    fn inline_keeps_a_fields_styles_on_one_line_without_its_block_marks() {
        let spans = inline("## Done\n\n- **bold** and `code`\n1. *two*\n\n---\n\n> quoted", Style::new().fg(Color::Gray));
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "Done · bold and code · two · quoted");
        let style_of = |text: &str| spans.iter().find(|s| s.content == text).unwrap_or_else(|| panic!("{text} in {spans:?}")).style;
        assert!(style_of("bold").add_modifier.contains(Modifier::BOLD));
        assert!(style_of("two").add_modifier.contains(Modifier::ITALIC));
        assert_eq!(style_of("code").fg, Some(CODE));
        assert_eq!(style_of("Done").fg, Some(Color::Gray));
        assert!(inline("", Style::new()).is_empty());
    }

    #[test]
    fn inline_styles_become_span_styles() {
        let line = &render(
            &parse("**b** *i* ***bi*** ~~s~~ `c` [t](https://docs.rs/x)"),
            80,
        )
        .lines[0];
        let style_of = |text: &str| {
            line.spans
                .iter()
                .find(|s| s.content == text)
                .unwrap_or_else(|| panic!("{text} in {line:?}"))
                .style
        };
        let cases = [
            ("b", Modifier::BOLD),
            ("i", Modifier::ITALIC),
            ("bi", Modifier::BOLD | Modifier::ITALIC),
            ("s", Modifier::CROSSED_OUT),
            ("t", Modifier::UNDERLINED),
        ];
        for (text, m) in cases {
            assert!(
                style_of(text).add_modifier.contains(m),
                "{text}: {:?}",
                style_of(text)
            );
        }
        assert_eq!(style_of("c").fg, Some(CODE));
        assert_eq!(style_of("(docs.rs)").fg, Some(DIM));
    }

    #[test]
    fn a_link_shows_its_host_unless_the_text_already_is_the_url() {
        let cases = [
            (
                "[the guide](https://www.docs.rs/a?b)",
                "the guide (docs.rs)",
            ),
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

    fn targets(md: &str) -> Vec<Vec<(TargetKind, String, String)>> {
        parse(md)
            .into_iter()
            .map(|b| {
                b.targets
                    .into_iter()
                    .map(|t| (t.kind, t.url, t.label))
                    .collect()
            })
            .collect()
    }

    fn link(url: &str, label: &str) -> (TargetKind, String, String) {
        (TargetKind::Link, url.into(), label.into())
    }

    fn image(url: &str, label: &str) -> (TargetKind, String, String) {
        (TargetKind::Image, url.into(), label.into())
    }

    #[test]
    fn markdown_links_and_images_keep_what_they_open() {
        assert_eq!(
            targets("See [the guide](https://docs.rs/a) and <https://x.y>, ![shot](https://x.y/s.png) or <me@x.y>."),
            vec![vec![
                link("https://docs.rs/a", "the guide"),
                link("https://x.y", "https://x.y"),
                image("https://x.y/s.png", "shot"),
                link("mailto:me@x.y", "me@x.y"),
            ]]
        );
        assert_eq!(
            rendered("![shot](https://x.y/s.png) ![](https://x.y/logo.svg?v=2)", 80),
            vec!["📷 shot (x.y) 📷 logo.svg (x.y)"]
        );
    }

    #[test]
    fn an_image_in_a_table_cell_opens_like_a_loose_one() {
        let stored = format!("/files?path=files/images/{}.png", "a".repeat(64));
        let table = format!("| shot |\n|---|\n| ![paywall]({stored}) |");
        let loose = format!("![paywall]({stored})");
        let opened = |md: &str| -> Vec<Option<String>> {
            parse(md).iter().flat_map(|b| b.targets.iter()).map(|t| t.stored().map(str::to_owned)).collect()
        };
        assert_eq!(opened(&table), opened(&loose));
        assert_eq!(opened(&table), vec![Some(stored)]);
        assert_eq!(rendered(&table, 80).join(" ").split("───────").last().map(str::trim), Some("📷 paywall"));
    }

    #[test]
    fn html_images_and_links_read_like_markdown_ones() {
        let cases = [
            (r#"<img src="https://x.y/a.png" alt="Logo" width=120>"#, "📷 Logo (x.y)"),
            (r#"<img width='120' alt='Logo' src='https://x.y/a.png'/>"#, "📷 Logo (x.y)"),
            ("<IMG ALT=Logo SRC=https://x.y/a.png>", "📷 Logo (x.y)"),
            (r#"Read <a href="https://x.y/docs">the docs</a> first."#, "Read the docs (x.y) first."),
        ];
        for (md, shown) in cases {
            assert_eq!(rendered(md, 80), vec![shown], "{md}");
            let expected = if md.contains("<a") {
                link("https://x.y/docs", "the docs")
            } else {
                image("https://x.y/a.png", "Logo")
            };
            assert_eq!(targets(md), vec![vec![expected]], "{md}");
        }
    }

    #[test]
    fn an_image_inside_a_link_opens_both_and_its_text_links_to_the_link() {
        let md = r#"<a href="https://ci.x/run"><img alt="build" src="https://img.x/b.svg"></a> and [![tests](https://img.x/t.svg)](https://ci.x/run)"#;
        assert_eq!(
            targets(md),
            vec![vec![
                link("https://ci.x/run", "build"),
                image("https://img.x/b.svg", "build"),
                image("https://img.x/t.svg", "tests"),
            ]]
        );
        assert_eq!(rendered(md, 80), vec!["📷 build (ci.x) and 📷 tests (ci.x)"]);
        let blocks = parse(md);
        let hrefs: Vec<(String, Option<u8>)> = blocks[0].lines[0]
            .iter()
            .filter(|r| !r.fmt.host)
            .map(|r| (r.text.clone(), r.fmt.href))
            .collect();
        assert_eq!(
            hrefs,
            vec![
                ("📷 ".into(), Some(0)),
                ("build".into(), Some(0)),
                (" and ".into(), None),
                ("📷 ".into(), Some(0)),
                ("tests".into(), Some(0)),
            ]
        );
    }

    #[test]
    fn layout_html_goes_and_a_block_of_only_layout_leaves_nothing() {
        let md = "<!-- badges -->\n\n<p align=\"center\">\n  <br/>\n</p>\n\n<div align=center>\n<img\n  src=\"https://x.y/w.png\"\n  alt=\"Wide\">\n</div>\n\n<details><summary>More</summary>\n\nBody\n\n</details>\n";
        assert_eq!(rendered(md, 80), vec!["📷 Wide (x.y)", "", "More", "", "Body"]);
        assert_eq!(targets(md)[0], vec![image("https://x.y/w.png", "Wide")]);
    }

    #[test]
    fn block_tags_and_br_break_lines_and_b_i_style_them() {
        let md = "<h1 align=center>Alondra</h1>\n<p>One<br>two <b>bold</b> <em>it</em> &amp; more</p>";
        assert_eq!(rendered(md, 80), vec!["Alondra", "One", "two bold it & more"]);
        let runs = &parse(md)[0].lines[2];
        assert!(runs.iter().any(|r| r.text == "bold" && r.fmt.bold));
        assert!(runs.iter().any(|r| r.text == "it" && r.fmt.italic));
    }

    #[test]
    fn html_indentation_and_line_breaks_read_as_single_spaces() {
        let md = "<p align=\"center\">\n  <a href=\"https://x.y/a\">Docs</a>   ·\n    <a href=\"https://z.w/b\">Code</a>\n</p>";
        assert_eq!(rendered(md, 80), vec!["Docs (x.y) · Code (z.w)"]);
    }

    #[test]
    fn picture_sources_give_way_to_their_image_and_unknown_tags_stay_text() {
        let md = "<picture>\n  <source srcset=\"https://x.y/dark.png\">\n  <img src=\"https://x.y/light.png\" alt=\"Diagram\">\n</picture>\n\nPress <kbd>v</kbd>, 3 < 4.\n\n<table><tr><td>x</td></tr></table>\n";
        assert_eq!(
            rendered(md, 80),
            vec![
                "📷 Diagram (x.y)",
                "",
                "Press <kbd>v</kbd>, 3 < 4.",
                "",
                "<table><tr><td>x</td></tr></table>",
            ]
        );
        assert_eq!(targets(md)[0], vec![image("https://x.y/light.png", "Diagram")]);
    }

    #[test]
    fn a_local_image_says_it_cannot_be_shown_here() {
        assert_eq!(
            rendered("![main screen](docs/main.png)", 80),
            vec!["📷 main screen (imagen local, no disponible aquí)"]
        );
        let target = &parse("<img src=\"docs/main.png\">")[0].targets[0];
        assert!(target.local() && !target.openable());
        assert_eq!(target.label, "main.png");
    }

    #[test]
    fn an_image_alondra_copied_shows_as_an_image_and_opens_from_the_daemon() {
        let route = format!("/files?path=files/images/{}.png", "a".repeat(64));
        assert_eq!(rendered(&format!("![main screen]({route})"), 80), vec!["📷 main screen"]);
        assert_eq!(rendered(&format!("<img src=\"{route}\">"), 80), vec!["📷 imagen"]);
        let target = &parse(&format!("![main screen]({route})"))[0].targets[0];
        assert!(!target.local() && target.openable());
        assert_eq!(target.stored(), Some(route.as_str()));
        let forged = &parse("![x](/files?path=/etc/hosts.png)")[0].targets[0];
        assert!(forged.stored().is_none() && !forged.openable());
    }

    #[test]
    fn only_web_and_mail_targets_open() {
        let open = |url: &str| {
            Target {
                kind: TargetKind::Link,
                url: url.into(),
                label: String::new(),
            }
            .openable()
        };
        assert!(open("https://x.y") && open("HTTP://x.y") && open("mailto:a@x.y"));
        assert!(!open("file:///etc/passwd") && !open("javascript:alert(1)") && !open("notes.md"));
        assert!(!open("https://x.y/\u{1b}]8;;evil"));
    }

    #[test]
    fn hyperlinks_wrap_each_linked_cell_and_untag_the_rest() {
        use ratatui::widgets::{Paragraph, Widget};
        let blocks = parse("Go [here](https://x.y/a) or [there](notes.md).");
        let lines = render_linked(&blocks, 40).lines;
        assert!(render(&blocks, 40).lines.iter().flat_map(|l| &l.spans).all(|s| s.style.underline_color.is_none()));
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        Paragraph::new(lines.clone()).render(area, &mut buf);
        let mut plain = buf.clone();
        hyperlinks(&mut plain, area, &blocks, false);
        assert!(plain.content.iter().all(|c| c.underline_color == Color::Reset));
        assert_eq!(plain.content[3].symbol(), "h");
        hyperlinks(&mut buf, area, &blocks, true);
        let linked: Vec<usize> = (0..40).filter(|&x| buf.content[x].symbol().starts_with('\u{1b}')).collect();
        assert_eq!(linked, vec![3, 4, 5, 6]);
        assert_eq!(buf.content[3].symbol(), "\u{1b}]8;;https://x.y/a\u{1b}\\h\u{1b}]8;;\u{1b}\\");
        assert!(buf.content.iter().all(|c| c.underline_color == Color::Reset));
    }

    #[test]
    fn a_hard_break_starts_a_new_line_under_the_same_prefix() {
        assert_eq!(
            rendered("- one  \n  two\\\n  three", 40),
            vec!["• one", "  two", "  three"]
        );
        assert_eq!(parse("one  \ntwo")[0].quote(), "one two");
    }

    const FLOW: &str = "flowchart TD\n  A[Card arrives] --> B{Needs user?}\n  B -->|yes| C[Ask Alice]\n  B -->|no| D[Run node]\n  C --> E[Deliver result]\n  D --> E";

    const FLOW_DRAWN: &[&str] = &[
        "         ┌──────────────┐",
        "         │ Card arrives │",
        "         └──────────────┘",
        "                 │",
        "                 │",
        "                 │",
        "                 │",
        "                 ▼",
        "          ┌─────────────┐",
        "          < Needs user? >",
        "          └─────────────┘",
        "      ┌────┘           └───┐",
        "      │                    │",
        "     yes                  no",
        "      │                    │",
        "      │                    │",
        "      │                    │",
        "      │                    │",
        "      │                    │",
        "      ▼                    ▼",
        "┌───────────┐        ┌──────────┐",
        "│ Ask Alice │        │ Run node │",
        "└───────────┘        └──────────┘",
        "        │                │",
        "        └┐              ┌┘",
        "         │              │",
        "         │              │",
        "         ▼              ▼",
        "        ┌────────────────┐",
        "        │ Deliver result │",
        "        └────────────────┘",
    ];

    const TOO_WIDE: &str = "flowchart LR\n  P[Prepare the release notes] --> R[Release] --> A[Build the archive] & B[Sign the binaries] & C[Run the smoke tests] & D[Rehearse the upgrade] & E[Publish the formula]";

    fn fenced(source: &str) -> String {
        format!("```mermaid\n{source}\n```")
    }

    fn framed(diagram: &[&str]) -> Vec<String> {
        std::iter::once("mermaid".to_string())
            .chain(diagram.iter().map(|l| format!("│ {l}").trim_end().to_string()))
            .collect()
    }

    fn shown_as_code(source: &str, notice: &str, width: usize) -> Vec<String> {
        let mut out = vec!["mermaid".to_string(), notice.to_string()];
        out.extend(rendered(&format!("```\n{source}\n```"), width));
        out
    }

    #[test]
    fn a_flowchart_draws_as_a_diagram_at_80_columns() {
        assert_eq!(rendered(&fenced(FLOW), 80), framed(FLOW_DRAWN));
    }

    #[test]
    fn a_sequence_diagram_draws_at_80_columns() {
        let source = "sequenceDiagram\n  participant TUI\n  participant Daemon\n  participant Node\n  TUI->>Daemon: open task\n  Daemon->>Node: start session\n  Node-->>Daemon: result ready\n  Daemon-->>TUI: show card";
        assert_eq!(
            rendered(&fenced(source), 80),
            framed(&[
                "┌─────┐           ┌────────┐           ┌──────┐",
                "│ TUI │           │ Daemon │           │ Node │",
                "└──┬──┘           └────┬───┘           └───┬──┘",
                "   │                   │                   │",
                "   │─open task────────>│                   │",
                "   │                   │                   │",
                "   │                   │─start session────>│",
                "   │                   │                   │",
                "   │                   │<┄result ready┄┄┄┄┄│",
                "   │                   │                   │",
                "   │<┄show card┄┄┄┄┄┄┄┄│                   │",
                "   │                   │                   │",
            ])
        );
    }

    #[test]
    fn a_state_diagram_draws_at_80_columns() {
        let source = "stateDiagram-v2\n  [*] --> Queued\n  Queued --> Working: capacity\n  Working --> Waiting: asks\n  Waiting --> Working: answered\n  Working --> [*]";
        assert_eq!(
            rendered(&fenced(source), 80),
            framed(&[
                "     ●",
                "",
                "     │",
                "     │",
                "     ▼",
                "╭────────╮",
                "│ Queued │",
                "╰────────╯",
                "     │",
                "     │",
                "     │",
                " capacity",
                "     │",
                "     ▼",
                "╭─────────╮",
                "│ Working │◄─────┐",
                "╰─────────╯      │",
                " │   └───┐       │",
                " │       │       │",
                " ▼       │   answered",
                "         │       │",
                "◉      asks      │",
                "     ┌───┘       │",
                "     │           │",
                "     │           │",
                "     ▼           │",
                "    ╭─────────╮──┘",
                "    │ Waiting │",
                "    ╰─────────╯",
            ])
        );
    }

    #[test]
    fn a_class_diagram_draws_at_80_columns() {
        let source = "classDiagram\n  class Task {\n    +String id\n    +close()\n  }\n  class Plan {\n    +Vec~Step~ steps\n  }\n  Task \"1\" --> \"1\" Plan";
        assert_eq!(
            rendered(&fenced(source), 80),
            framed(&[
                "   ┌────────────┐",
                "   │    Task    │",
                "   ├────────────┤",
                "   │ +String id │",
                "   ├────────────┤",
                "   │ +close()   │",
                "   └────────────┘",
                "        1 │",
                "          │",
                "        1 ▼",
                "┌──────────────────┐",
                "│       Plan       │",
                "├──────────────────┤",
                "│ +Vec~Step~ steps │",
                "├──────────────────┤",
                "└──────────────────┘",
            ])
        );
    }

    #[test]
    fn a_left_to_right_flowchart_that_does_not_fit_is_redrawn_top_down() {
        let source = FLOW.replacen("flowchart TD", "flowchart LR", 1);
        let narrow = rendered(&fenced(&source), 60);
        assert_eq!(narrow, framed(FLOW_DRAWN));
        assert!(narrow.iter().all(|l| str_width(l) <= 60));
        let wide = rendered(&fenced(&source), 120);
        assert_ne!(wide, narrow);
        assert!(wide.iter().any(|l| str_width(l) > 60));
        assert!(wide.iter().all(|l| str_width(l) <= 120));
    }

    #[test]
    fn a_diagram_that_does_not_fit_even_top_down_shows_its_code_with_a_notice() {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        assert_eq!(
            rendered(&fenced(TOO_WIDE), 80),
            shown_as_code(TOO_WIDE, "The diagram doesn't fit this width; widen the window", 80)
        );
    }

    #[test]
    fn invalid_syntax_shows_its_code_with_the_line_of_the_error() {
        let source = "flowchart TD\n  A -->\n  B[[[ -->";
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        assert_eq!(
            rendered(&fenced(source), 80),
            shown_as_code(source, "This diagram has an error on line 2; showing its code", 80)
        );
        crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
        assert_eq!(
            rendered(&fenced(source), 80),
            shown_as_code(source, "El diagrama tiene un error en la línea 2; se muestra su código", 80)
        );
    }

    #[test]
    fn pie_and_gantt_show_their_code_with_a_notice() {
        for source in ["pie\n  \"Done\" : 3\n  \"Left\" : 1", "gantt\n  title Release\n  section Build\n  Archive :a1, 2026-10-01, 2d"] {
            crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
            assert_eq!(
                rendered(&fenced(source), 80),
                shown_as_code(source, "This kind of diagram isn't drawn here yet", 80)
            );
            crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
            assert_eq!(
                rendered(&fenced(source), 80),
                shown_as_code(source, "Este tipo de diagrama aún no se dibuja aquí", 80)
            );
        }
    }

    #[test]
    fn more_than_a_hundred_nodes_show_their_code_with_a_notice() {
        let source = std::iter::once("flowchart TD".to_string())
            .chain((0..=100).map(|i| format!("  N{i}")))
            .collect::<Vec<_>>()
            .join("\n");
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        assert_eq!(
            rendered(&fenced(&source), 80),
            shown_as_code(&source, "Diagram too large to draw here", 80)
        );
        crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
        assert_eq!(rendered(&fenced(&source), 80)[1], "Diagrama demasiado grande para dibujarlo aquí");
        assert_eq!(rendered(&fenced(TOO_WIDE), 80)[1], "El diagrama no cabe en este ancho; amplía la ventana");
    }

    #[test]
    fn quoting_a_drawn_diagram_quotes_its_code() {
        let block = &parse(&fenced(FLOW))[0];
        assert!(block.commentable());
        assert_eq!(
            block.quote(),
            "flowchart TD A[Card arrives] --> B{Needs user?} B -->|yes| C[Ask Alice] B -->|no| D[Run node] C --> E[Deliver result] D --> E"
        );
    }

    #[test]
    fn the_mermaid_fixtures_draw_or_show_their_code() {
        let fixture = |name: &str| {
            let path = corpus::fixtures_dir().join(format!("{name}.md"));
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        };
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        let flow = rendered(&fixture("mermaid_flowchart"), 80);
        assert!(flow.iter().any(|l| l == "│          │ Card arrives │"), "{flow:#?}");
        let other = rendered(&fixture("mermaid_unsupported"), 80);
        assert!(other.iter().any(|l| l == "This kind of diagram isn't drawn here yet"), "{other:#?}");
        assert!(other.iter().any(|l| l == "│ pie title Where the time goes"), "{other:#?}");
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
        assert_eq!(
            parse("my_var_x and 2 * 3 * 4")[0].plain(),
            "my_var_x and 2 * 3 * 4"
        );
        assert_eq!(parse(r"52\. two \*not\*")[0].plain(), "52. two *not*");
    }

    #[test]
    fn feedback_has_the_exact_format_the_nodes_expect() {
        let f = feedback(
            &[
                ("Step 2".into(), "Do it before\nstep 1".into()),
                ("Table".into(), "Not needed".into()),
            ],
            "Otherwise, fine.",
            "Ana",
        );
        assert_eq!(
            f,
            "Comments from Ana:\n\n1. On \"Step 2\":\n   Do it before\n   step 1\n\n2. On \"Table\":\n   Not needed\n\nOtherwise, fine."
        );
        assert_eq!(
            feedback(&[], "  Just this  ", "Ana"),
            "Comments from Ana:\n\nJust this"
        );
    }

    #[test]
    fn wrap_respects_width_prefixes_and_long_words() {
        let lines = wrap(
            vec![Span::raw(
                "one two three four supercalifragilisticexpialidocious",
            )],
            12,
            vec![Span::raw("• ")],
            vec![Span::raw("  ")],
        );
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
