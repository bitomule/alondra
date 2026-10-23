use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use mmdflux::builtins::default_registry;
use mmdflux::payload::Diagram;
use mmdflux::{OutputFormat, RenderConfig, render_diagram, validate_diagram};

use super::str_width;

pub const MAX_NODES: usize = 100;
const CACHE_SIZE: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    Invalid {
        line: Option<usize>,
        column: Option<usize>,
    },
    Unsupported,
    TooWide,
    TooLarge,
}

impl Refusal {
    pub fn notice(&self) -> String {
        match self {
            Refusal::Invalid { line: Some(line), .. } => trf!(
                "This diagram has an error on line {line}; showing its code",
                "El diagrama tiene un error en la línea {line}; se muestra su código"
            ),
            Refusal::Invalid { line: None, .. } => tr!(
                "This diagram has an error; showing its code",
                "El diagrama tiene un error; se muestra su código"
            )
            .to_string(),
            Refusal::Unsupported => tr!(
                "This kind of diagram isn't drawn here yet",
                "Este tipo de diagrama aún no se dibuja aquí"
            )
            .to_string(),
            Refusal::TooWide => tr!(
                "The diagram doesn't fit this width; widen the window",
                "El diagrama no cabe en este ancho; amplía la ventana"
            )
            .to_string(),
            Refusal::TooLarge => tr!(
                "Diagram too large to draw here",
                "Diagrama demasiado grande para dibujarlo aquí"
            )
            .to_string(),
        }
    }
}

type Drawing = Result<Vec<String>, Refusal>;

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::new(CACHE_SIZE));
}

pub fn draw(source: &str, width: usize) -> Drawing {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    let key = (hasher.finish(), width);
    CACHE.with(|cache| cache.borrow_mut().get_or_insert(key, || draw_uncached(source, width)))
}

fn draw_uncached(source: &str, width: usize) -> Drawing {
    let registry = default_registry();
    let id = registry.detect(source).ok_or(Refusal::Unsupported)?;
    let instance = registry.create(id).ok_or(Refusal::Unsupported)?;
    let payload = instance
        .parse(source)
        .map_err(|_| invalid(source))?
        .into_payload()
        .map_err(|_| invalid(source))?;
    if nodes(&payload) > MAX_NODES {
        return Err(Refusal::TooLarge);
    }
    let lines = text(source)?;
    if fits(&lines, width) {
        return Ok(lines);
    }
    let lines = top_down(source)
        .map(|source| text(&source))
        .transpose()?
        .ok_or(Refusal::TooWide)?;
    if fits(&lines, width) {
        Ok(lines)
    } else {
        Err(Refusal::TooWide)
    }
}

fn nodes(payload: &Diagram) -> usize {
    match payload {
        Diagram::Flowchart(graph) | Diagram::Class(graph) | Diagram::State(graph) => {
            graph.nodes.len()
        }
        Diagram::Sequence(sequence) => sequence.participants.len(),
    }
}

fn text(source: &str) -> Drawing {
    let drawn = render_diagram(source, OutputFormat::Text, &RenderConfig::default())
        .map_err(|_| invalid(source))?;
    let mut lines: Vec<String> = drawn.lines().map(|l| l.trim_end().to_string()).collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    Ok(lines)
}

fn fits(lines: &[String], width: usize) -> bool {
    lines.iter().all(|l| str_width(l) <= width)
}

fn invalid(source: &str) -> Refusal {
    let report: serde_json::Value =
        serde_json::from_str(&validate_diagram(source)).unwrap_or_default();
    let at = |field: &str| {
        report["diagnostics"][0][field]
            .as_u64()
            .map(|n| n as usize)
    };
    Refusal::Invalid {
        line: at("line"),
        column: at("column"),
    }
}

fn top_down(source: &str) -> Option<String> {
    let mut changed = false;
    let lines: Vec<String> = source
        .lines()
        .map(|line| {
            let mut words: Vec<String> = line.split_whitespace().map(str::to_string).collect();
            let horizontal = |w: &str| matches!(w.trim_end_matches(';'), "LR" | "RL");
            let turned = match words.as_mut_slice() {
                [kind, dir, ..] if (kind == "flowchart" || kind == "graph") && horizontal(dir) => {
                    *dir = format!("TD{}", &dir[2..]);
                    true
                }
                [kind, dir] if kind == "direction" && horizontal(dir) => {
                    *dir = "TB".to_string();
                    true
                }
                _ => false,
            };
            if !turned {
                return line.to_string();
            }
            changed = true;
            let indent = &line[..line.len() - line.trim_start().len()];
            format!("{indent}{}", words.join(" "))
        })
        .collect();
    changed.then(|| lines.join("\n"))
}

struct Cache {
    capacity: usize,
    entries: Vec<((u64, usize), Drawing)>,
}

impl Cache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Vec::with_capacity(capacity),
        }
    }

    fn get_or_insert(&mut self, key: (u64, usize), draw: impl FnOnce() -> Drawing) -> Drawing {
        if let Some(i) = self.entries.iter().position(|(k, _)| *k == key) {
            let entry = self.entries.remove(i);
            self.entries.push(entry);
        } else {
            if self.entries.len() == self.capacity {
                self.entries.drain(..1);
            }
            self.entries.push((key, draw()));
        }
        self.entries.last().map(|(_, d)| d.clone()).expect("just pushed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_left_to_right_header_and_direction_lines_turn_top_down() {
        assert_eq!(
            top_down("%% c\n  flowchart LR\n  A --> B\n  subgraph S\n    direction RL\n  end").as_deref(),
            Some("%% c\n  flowchart TD\n  A --> B\n  subgraph S\n    direction TB\n  end")
        );
        assert_eq!(top_down("graph LR;\nA-->B").as_deref(), Some("graph TD;\nA-->B"));
        assert_eq!(top_down("graph RL\nA-->B").as_deref(), Some("graph TD\nA-->B"));
        assert_eq!(top_down("flowchart TD\nA-->B"), None);
    }

    #[test]
    fn the_cache_draws_once_per_key_and_forgets_the_oldest_beyond_its_size() {
        let mut cache = Cache::new(2);
        let mut calls = 0;
        let mut get = |cache: &mut Cache, key| {
            cache.get_or_insert(key, || {
                calls += 1;
                Ok(vec![format!("{key:?}")])
            })
        };
        let keys = [(1, 80), (1, 80), (1, 60), (1, 80), (2, 80), (1, 80), (1, 60)];
        let drawn: Vec<Drawing> = keys.iter().map(|&key| get(&mut cache, key)).collect();
        let expected: Vec<Drawing> = keys.iter().map(|key| Ok(vec![format!("{key:?}")])).collect();
        assert_eq!(drawn, expected);
        assert_eq!(calls, 4);
    }

    #[test]
    fn more_than_a_hundred_nodes_is_too_large() {
        let mut source = String::from("flowchart TD\n");
        for i in 0..=MAX_NODES {
            source.push_str(&format!("  N{i}\n"));
        }
        assert_eq!(draw_uncached(&source, 10_000), Err(Refusal::TooLarge));
        let at_limit: String = source.lines().take(MAX_NODES + 1).collect::<Vec<_>>().join("\n");
        assert!(draw_uncached(&at_limit, 10_000).is_ok());
    }

    #[test]
    fn invalid_syntax_reports_its_line_and_column() {
        assert_eq!(
            draw_uncached("flowchart TD\n  A --> \n  B[[[ --> ", 80),
            Err(Refusal::Invalid {
                line: Some(2),
                column: Some(9)
            })
        );
    }

    #[test]
    fn other_kinds_are_unsupported() {
        for source in ["pie\n  \"a\": 1", "gantt\n  title x", "erDiagram\n  A ||--o{ B : has"] {
            assert_eq!(draw_uncached(source, 80), Err(Refusal::Unsupported), "{source}");
        }
    }
}
