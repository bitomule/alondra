//! The sheets of a spec and of a decision as the terminal draws them: the spec's frame, each
//! scenario with its steps in lanes and its tables as tables, diagrams drawn, and each mockup or
//! capture as a framed line `o` opens in the Mac's image viewer. In the reader every part takes a
//! comment of its own, and `z` folds and unfolds.
use super::md::{str_width, Block, Kind, Target, TargetKind};
use alondra_view_model::{
    DecisionSheet, Icon, SheetAnchor, SheetLane, SheetLine, SheetMark, SheetPart, SheetPartKind, SheetReferences, SheetResult, SheetScenario, SpecSheet, Tone,
};
use ratatui::style::Color;
use std::collections::BTreeSet;

const DIM: Color = Color::DarkGray;
const ACCENT: Color = Color::LightYellow;

fn color(tone: Tone) -> Color {
    match tone {
        Tone::Success => Color::Green,
        Tone::Warning => Color::Yellow,
        Tone::Critical => Color::Red,
        Tone::Info | Tone::Accent => Color::Cyan,
        Tone::YourTurn => ACCENT,
        Tone::Neutral => Color::Reset,
    }
}

fn glyph(icon: Icon) -> &'static str {
    match icon {
        Icon::Check => "✓",
        Icon::Warning => "!",
        Icon::Notice => "∅",
        Icon::History => "…",
        Icon::Question => "?",
        _ => "▤",
    }
}

fn marked(mut words: Vec<String>, mark: Option<&SheetMark>) -> String {
    words.extend(mark.map(|mark| mark.label.clone()));
    words.retain(|word| !word.is_empty());
    words.join(" · ")
}

struct Drawing<'a> {
    folds: &'a BTreeSet<String>,
    blocks: Vec<Block>,
}

impl Drawing<'_> {
    fn push(&mut self, block: Block) {
        self.blocks.push(block);
    }

    fn note(&mut self, color: Color, depth: usize, text: &str, quote: Option<&str>) {
        let block = Block::of_sheet(Kind::Toned(color), depth, text);
        self.push(match quote {
            Some(quote) => block.saying(quote),
            None => block,
        });
    }

    fn table(&mut self, rows: &[Vec<String>], depth: usize, quote: Option<&str>) {
        for (n, cells) in rows.iter().enumerate() {
            let block = Block::row(n == 0 && rows.len() > 1, cells, depth);
            self.push(match quote {
                Some(quote) => block.saying(quote),
                None => block,
            });
        }
    }

    fn lanes(&mut self, lanes: &[SheetLane], anchor: Option<&SheetAnchor>, depth: usize) {
        let column = lanes.iter().map(|lane| str_width(&lane.label)).max().unwrap_or(0) as u16;
        for lane in lanes {
            for (n, step) in lane.steps.iter().enumerate() {
                let label = if n == 0 { lane.label.clone() } else { String::new() };
                let mut block = Block::of_sheet(Kind::Lane { label, width: column }, depth, &step.text);
                if let Some(anchor) = anchor {
                    block = block.saying(&format!("{} — {} {}", anchor.quote, lane.label, step.text));
                }
                self.push(block);
                let inside = depth + usize::from(column + 2).div_ceil(2);
                if !step.table.is_empty() {
                    self.table(&step.table, inside, anchor.map(|anchor| anchor.quote.as_str()));
                }
                if let Some(doc) = &step.doc {
                    let mut block = Block::of_sheet(Kind::Code(None), inside, doc);
                    if let Some(anchor) = anchor {
                        block = block.saying(&anchor.quote);
                    }
                    self.push(block);
                }
            }
        }
    }

    fn part(&mut self, part: &SheetPart, depth: usize) {
        let quote = part.anchor.as_ref().map(|anchor| anchor.quote.as_str());
        match (part.kind, &part.code, &part.picture) {
            (SheetPartKind::Diagram, Some(code), _) => {
                self.note(Color::Cyan, depth, &marked(vec![format!("◇ {}", part.title)], part.mark.as_ref()), quote);
                let mut block = Block::of_sheet(Kind::Diagram, depth, code.trim_end());
                if let Some(quote) = quote {
                    block = block.saying(quote);
                }
                self.push(block);
            }
            (_, _, Some(picture)) => {
                let size = if picture.width > 0 { format!(" {}×{}", picture.width, picture.height) } else { String::new() };
                let text = marked(vec![format!("▣ {}{size}", part.title)], part.mark.as_ref());
                let hint = tr!("o opens it", "o la abre");
                let block = Block::of_sheet(Kind::Toned(ACCENT), depth, &format!("{text} · {hint}"));
                let block = match quote {
                    Some(quote) => block.saying(quote),
                    None => block,
                };
                self.push(block.opening(Target { kind: TargetKind::Image, url: picture.url.clone(), label: part.title.clone() }));
            }
            _ => {
                let why = part.unavailable.clone().unwrap_or_else(|| tr!("It cannot be shown", "No se puede mostrar").into());
                self.note(Color::Yellow, depth, &format!("∅ {} — {why}", part.title), quote);
            }
        }
    }

    fn line(&mut self, line: &SheetLine, depth: usize, quote: Option<&str>) {
        let mut text = format!("{} {}", glyph(line.icon), line.text);
        if let Some(detail) = &line.detail {
            text.push_str(&format!(" · {detail}"));
        }
        self.note(color(line.tone), depth, &text, quote);
    }

    fn result(&mut self, result: &SheetResult, quote: Option<&str>, depth: usize) {
        for line in result.checks.iter().chain(&result.tests) {
            self.line(line, depth, quote);
        }
        for verdict in &result.verdicts {
            self.line(&verdict.line, depth, quote);
            if let Some(reasoning) = &verdict.reasoning {
                self.push(Block::of_sheet(Kind::Quote, depth + 2, reasoning));
            }
            if let (Some(label), Some(nearest)) = (&verdict.nearest_label, &verdict.nearest_wrong) {
                self.note(DIM, depth + 1, &format!("{label}:"), None);
                self.push(Block::of_sheet(Kind::Quote, depth + 2, nearest));
            }
            for evidence in &verdict.evidence {
                self.part(evidence, depth + 1);
            }
        }
    }

    fn scenario(&mut self, scenario: &SheetScenario) {
        let open = scenario.collapsed == self.folds.contains(&scenario.id);
        let state = scenario.result.as_ref().map(|result| &result.state);
        let mut words = vec![scenario.kind.clone()];
        words.extend(state.map(|state| format!("{} {}", glyph(state.icon), state.text)));
        let quote = scenario.anchor.as_ref().map(|anchor| anchor.quote.as_str());
        let mut head = Block::of_sheet(Kind::Head { glyph: if open { "▾" } else { "▸" }, right: marked(words, scenario.mark.as_ref()) }, 1, &scenario.title);
        if let Some(quote) = quote {
            head = head.saying(quote);
        }
        head.fold = Some(scenario.id.clone());
        self.push(head);
        if !open {
            return;
        }
        self.lanes(&scenario.lanes, scenario.anchor.as_ref(), 3);
        if let Some(examples) = &scenario.examples {
            self.note(DIM, 3, &examples.label, None);
            self.table(&examples.rows, 3, quote);
        }
        for part in &scenario.parts {
            self.part(part, 3);
        }
        if let Some(result) = &scenario.result {
            self.result(result, quote, 3);
        }
    }

    fn references(&mut self, references: &SheetReferences, quote: Option<&str>) {
        let titles = references.items.iter().map(|item| format!("“{}”", item.title)).collect::<Vec<_>>().join(", ");
        self.note(DIM, 1, &format!("{}: {titles}", references.label), quote);
    }
}

/// The blocks of a spec's sheet; `folds` holds what the reader folded or unfolded from how the
/// daemon sends it.
pub fn spec_blocks(sheet: &SpecSheet, folds: &BTreeSet<String>) -> Vec<Block> {
    let quote = sheet.anchor.as_ref().map(|anchor| anchor.quote.as_str());
    let mut drawing = Drawing { folds, blocks: vec![] };
    let right = marked(vec![sheet.spec.clone()], sheet.mark.as_ref());
    let mut top = Block::of_sheet(Kind::Frame { right: Some(right) }, 0, &sheet.title);
    if let Some(quote) = quote {
        top = top.saying(quote);
    }
    drawing.push(top);
    if !sheet.explanation.is_empty() {
        let block = Block::of_sheet(Kind::Para, 1, &sheet.explanation);
        drawing.push(match quote {
            Some(quote) => block.saying(quote),
            None => block,
        });
    }
    if let Some(decisions) = &sheet.decisions {
        drawing.references(decisions, quote);
    }
    for part in &sheet.parts {
        drawing.part(part, 1);
    }
    if let Some(background) = &sheet.background {
        drawing.note(DIM, 1, &background.label, None);
        drawing.lanes(&background.lanes, sheet.anchor.as_ref(), 3);
    }
    if let Some(empty) = &sheet.empty {
        drawing.note(DIM, 1, empty, None);
    }
    for scenario in &sheet.scenarios {
        drawing.scenario(scenario);
    }
    if let Some(unchanged) = &sheet.unchanged {
        let id = format!("unchanged:{}", sheet.spec);
        let open = folds.contains(&id);
        let mut head = Block::of_sheet(Kind::Head { glyph: if open { "▾" } else { "·" }, right: String::new() }, 1, &unchanged.label);
        head.fold = Some(id);
        drawing.push(head);
        if open {
            for scenario in &unchanged.scenarios {
                drawing.scenario(scenario);
            }
        }
    }
    drawing.push(Block::of_sheet(Kind::Frame { right: None }, 0, ""));
    drawing.blocks
}

pub fn decision_blocks(sheet: &DecisionSheet) -> Vec<Block> {
    let quote = sheet.anchor.as_ref().map(|anchor| anchor.quote.as_str());
    let folds = BTreeSet::new();
    let mut drawing = Drawing { folds: &folds, blocks: vec![] };
    let right = marked(vec![sheet.decision.clone(), sheet.status.clone()], sheet.mark.as_ref());
    let mut top = Block::of_sheet(Kind::Frame { right: Some(right) }, 0, &sheet.title);
    if let Some(quote) = quote {
        top = top.saying(quote);
    }
    drawing.push(top);
    for section in &sheet.sections {
        drawing.note(Color::Cyan, 1, &section.label, None);
        let block = Block::of_sheet(Kind::Para, 2, &section.text);
        drawing.push(match quote {
            Some(quote) => block.saying(&format!("{quote} — {}", section.label)),
            None => block,
        });
    }
    for references in sheet.supersedes.iter().chain(&sheet.superseded_by) {
        drawing.references(references, quote);
    }
    drawing.push(Block::of_sheet(Kind::Frame { right: None }, 0, ""));
    drawing.blocks
}
