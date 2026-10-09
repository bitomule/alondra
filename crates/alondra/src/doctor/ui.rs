use std::io::Write;

use super::Status;

/// Ctrl-C or Esc inside a question: what was already done stays, and the run exits with 130.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(tr!("Cancelled.", "Cancelado."))
    }
}

impl std::error::Error for Cancelled {}

pub trait Progress {
    fn finish(self: Box<Self>);
}

/// How a repair talks to a person: the cliclack screens in a terminal, plain lines otherwise.
/// Only this layer knows the library, so replacing it touches one file.
pub trait Ui {
    fn intro(&mut self, title: &str);
    fn step(&mut self, status: Status, name: &str, detail: &str, fix: Option<&str>);
    fn note(&mut self, lines: &[String]);
    fn progress(&mut self, text: &str) -> Box<dyn Progress>;
    /// `None` when there is nobody to ask.
    fn confirm(&mut self, question: &str, default: bool) -> Result<Option<bool>, Cancelled>;
    fn select(&mut self, question: &str, options: &[String]) -> Result<Option<usize>, Cancelled>;
    /// Free text, such as a folder; `None` when there is nobody to ask.
    fn input(&mut self, question: &str) -> Result<Option<String>, Cancelled>;
    fn outro(&mut self, text: &str);
}

/// A step's name and detail on one line; a step may have only one of them.
fn line(name: &str, detail: &str, separator: &str) -> String {
    match (name.is_empty(), detail.is_empty()) {
        (true, _) => detail.to_string(),
        (_, true) => name.to_string(),
        _ => format!("{name}{separator}{detail}"),
    }
}

fn mark(status: Status) -> &'static str {
    match status {
        Status::Pass => "✓",
        Status::Warn => "!",
        Status::Fail => "✗",
    }
}

/// Lines without animation or color, for pipes, agents and `--no-input`. It never asks.
pub struct Plain<W: Write> {
    pub out: W,
}

struct Silent;

impl Progress for Silent {
    fn finish(self: Box<Self>) {}
}

impl<W: Write> Ui for Plain<W> {
    fn intro(&mut self, _title: &str) {}

    fn step(&mut self, status: Status, name: &str, detail: &str, fix: Option<&str>) {
        let _ = writeln!(self.out, "{} {}", mark(status), line(name, detail, ": "));
        if let Some(fix) = fix {
            let _ = writeln!(self.out, "  → {fix}");
        }
    }

    fn note(&mut self, lines: &[String]) {
        for line in lines {
            let _ = writeln!(self.out, "  {line}");
        }
    }

    fn progress(&mut self, _text: &str) -> Box<dyn Progress> {
        Box::new(Silent)
    }

    fn confirm(&mut self, _question: &str, _default: bool) -> Result<Option<bool>, Cancelled> {
        Ok(None)
    }

    fn select(&mut self, _question: &str, _options: &[String]) -> Result<Option<usize>, Cancelled> {
        Ok(None)
    }

    fn input(&mut self, _question: &str) -> Result<Option<String>, Cancelled> {
        Ok(None)
    }

    fn outro(&mut self, text: &str) {
        let _ = writeln!(self.out, "{text}");
    }
}

/// The clack look (cliclack), which writes to stderr. Only for a terminal on both ends.
pub struct Interactive {
    pub width: usize,
}

struct Spinner(cliclack::ProgressBar);

impl Progress for Spinner {
    fn finish(self: Box<Self>) {
        self.0.clear();
    }
}

fn answer<T>(result: std::io::Result<T>) -> Result<Option<T>, Cancelled> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => Err(Cancelled),
        Err(_) => Ok(None),
    }
}

impl Ui for Interactive {
    fn intro(&mut self, title: &str) {
        let _ = cliclack::intro(title);
    }

    fn step(&mut self, status: Status, name: &str, detail: &str, fix: Option<&str>) {
        let rule = "─".repeat(self.width.saturating_sub(name.chars().count()) + 2);
        let mut text = line(name, detail, &format!(" {rule} "));
        if let Some(fix) = fix {
            text.push_str(&format!("\n→ {fix}"));
        }
        let _ = match status {
            Status::Pass => cliclack::log::step(text),
            Status::Warn => cliclack::log::warning(text),
            Status::Fail => cliclack::log::error(text),
        };
    }

    fn note(&mut self, lines: &[String]) {
        let _ = cliclack::log::remark(lines.join("\n"));
    }

    fn progress(&mut self, text: &str) -> Box<dyn Progress> {
        let spinner = cliclack::spinner();
        spinner.start(text);
        Box::new(Spinner(spinner))
    }

    fn confirm(&mut self, question: &str, default: bool) -> Result<Option<bool>, Cancelled> {
        answer(cliclack::confirm(question).initial_value(default).interact())
    }

    fn select(&mut self, question: &str, options: &[String]) -> Result<Option<usize>, Cancelled> {
        let mut select = cliclack::select(question);
        for (i, option) in options.iter().enumerate() {
            select = select.item(i, option, "");
        }
        answer(select.interact())
    }

    fn input(&mut self, question: &str) -> Result<Option<String>, Cancelled> {
        answer(cliclack::input(question).interact())
    }

    fn outro(&mut self, text: &str) {
        let _ = cliclack::outro(text);
    }
}

/// A double that records every call and answers from a script.
#[cfg(test)]
#[derive(Default)]
pub struct Recorded {
    pub lines: Vec<String>,
    pub answers: Vec<Option<bool>>,
    pub choices: Vec<Option<usize>>,
    pub typed: Vec<Option<String>>,
}

#[cfg(test)]
struct Logged;

#[cfg(test)]
impl Progress for Logged {
    fn finish(self: Box<Self>) {}
}

#[cfg(test)]
impl Ui for Recorded {
    fn intro(&mut self, title: &str) {
        self.lines.push(format!("intro {title}"));
    }

    fn step(&mut self, status: Status, name: &str, detail: &str, fix: Option<&str>) {
        self.lines.push(format!("{} {}{}", mark(status), line(name, detail, ": "), fix.map(|f| format!(" → {f}")).unwrap_or_default()));
    }

    fn note(&mut self, lines: &[String]) {
        self.lines.extend(lines.iter().map(|l| format!("note {l}")));
    }

    fn progress(&mut self, text: &str) -> Box<dyn Progress> {
        self.lines.push(format!("progress {text}"));
        Box::new(Logged)
    }

    fn confirm(&mut self, question: &str, _default: bool) -> Result<Option<bool>, Cancelled> {
        self.lines.push(format!("ask {question}"));
        Ok(if self.answers.is_empty() { None } else { self.answers.remove(0) })
    }

    fn select(&mut self, question: &str, options: &[String]) -> Result<Option<usize>, Cancelled> {
        self.lines.push(format!("select {question} [{}]", options.join(" | ")));
        Ok(if self.choices.is_empty() { None } else { self.choices.remove(0) })
    }

    fn input(&mut self, question: &str) -> Result<Option<String>, Cancelled> {
        self.lines.push(format!("input {question}"));
        Ok(if self.typed.is_empty() { None } else { self.typed.remove(0) })
    }

    fn outro(&mut self, text: &str) {
        self.lines.push(format!("outro {text}"));
    }
}
