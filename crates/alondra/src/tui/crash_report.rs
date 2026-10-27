//! The crash report, from `:report`, Settings or a failed task's “Send crash report”: “What
//! happened?” first, in the composer; then the Mac's daemon builds the report and this screen shows
//! it, every file whole on Enter, exactly as it would leave the Mac; then one question, whether to
//! send it to the report repository. Nothing is sent until the user says yes.

use std::time::Duration;

use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use serde_json::{Value, json};

use super::keys::Motion;
use super::{App, Client, Msg, Target};
use crate::report::send::Kind;

const BUILD: Duration = Duration::from_secs(330);
const SEND: Duration = Duration::from_secs(16 * 60);
const ACCENT: Color = Color::LightCyan;
const DIM: Color = Color::DarkGray;

/// What the daemon answered about the report it built.
#[derive(Clone, Debug, PartialEq)]
pub struct Built {
    pub id: String,
    pub dir: String,
    pub compressed: u64,
    pub hours: u64,
    pub repo: Option<String>,
    pub passed: bool,
    pub hits: Vec<String>,
    /// Every file of the package, in order: its name and size, or why it has nothing.
    pub files: Vec<(String, Result<u64, String>)>,
    pub left_out: Vec<String>,
}

impl Built {
    pub fn of(answer: &Value) -> Option<Built> {
        let manifest = &answer["manifest"];
        let mut files = vec![("manifest.json".to_owned(), Err(String::new()))];
        if let Some(bytes) = manifest["description"]["bytes"].as_u64() {
            files.push(("description.txt".to_owned(), Ok(bytes)));
        }
        for section in manifest["sections"].as_array().into_iter().flatten() {
            let name = section["name"].as_str()?.to_owned();
            files.push(match section["bytes"].as_u64() {
                Some(bytes) if section["sha256"].is_string() => (name, Ok(bytes)),
                _ => (name, Err(section["empty"].as_str().unwrap_or_default().to_owned())),
            });
        }
        let mut left_out: Vec<String> = manifest["omitted"].as_object().into_iter().flatten()
            .filter(|(_, count)| count.as_u64().unwrap_or_default() > 0)
            .map(|(what, count)| format!("{}: {count}", crate::report::omitted_label(what)))
            .collect();
        for trim in manifest["trimmed"].as_array().into_iter().flatten() {
            let (lines, section) = (trim["lines"].as_u64().unwrap_or_default(), trim["section"].as_str().unwrap_or_default());
            left_out.push(trf!("cut to fit in 5 MB: the oldest {lines} lines of {section}", "recortado para que quepa en 5 MB: las {lines} líneas más antiguas de {section}"));
        }
        let hits = answer["check"]["hits"].as_array().into_iter().flatten().map(|hit| {
            let (section, line, what) = (hit["section"].as_str().unwrap_or_default(), hit["line"].as_u64().unwrap_or_default(), hit["what"].as_str().unwrap_or_default());
            trf!("{section}, line {line}: {what}", "{section}, línea {line}: {what}")
        }).collect();
        Some(Built {
            id: answer["id"].as_str().filter(|id| crate::report::bundle::is_id(id))?.to_owned(),
            dir: answer["dir"].as_str().unwrap_or_default().to_owned(),
            compressed: answer["compressed_bytes"].as_u64().unwrap_or_default(),
            hours: manifest["window"]["hours"].as_u64().unwrap_or(72),
            repo: answer["repo"].as_str().filter(|repo| alondra_core::config::is_github_repo(repo)).map(str::to_owned),
            passed: answer["check"]["passed"] == true,
            hits,
            files,
            left_out,
        })
    }

    /// The files the screen can open: those the package holds.
    pub fn openable(&self) -> Vec<&str> {
        self.files.iter().filter(|(name, size)| size.is_ok() || name == "manifest.json").map(|(name, _)| name.as_str()).collect()
    }

    /// Whether the one question can be asked: the final check passed and there is somewhere to send it.
    pub fn sendable(&self) -> bool {
        self.passed && self.repo.is_some()
    }
}

pub enum Stage {
    Building,
    Ready(Built),
    Sending(Built),
    Done(String),
    Failed(String),
}

/// A file of the report, shown whole.
pub struct Viewing {
    pub name: String,
    pub text: Option<Result<Vec<String>, String>>,
    pub scroll: usize,
}

pub struct CrashReport {
    pub failure: String,
    pub description: Option<String>,
    pub stage: Stage,
    pub sel: usize,
    pub scroll: usize,
    pub viewing: Option<Viewing>,
}

/// What the daemon answered for the report.
pub enum Answer {
    Built(Result<Value, String>),
    File { id: String, name: String, result: Result<Value, String> },
    Sent { id: String, result: Result<Value, String> },
}

fn lang() -> &'static str {
    match crate::i18n::lang() {
        crate::i18n::Lang::Es => "es",
        crate::i18n::Lang::En => "en",
    }
}

fn kind_label(failure: &str) -> &'static str {
    Kind::from_word(failure).unwrap_or(Kind::Other).label()
}

/// What sending came to: sent, pending until GitHub answers, or why nothing was sent.
pub fn sent_message(id: &str, result: Result<Value, String>) -> String {
    let message = |answer: &Value| answer["message"].as_str().unwrap_or_default().to_owned();
    let unsent = tr!("Nothing was sent; it stays on this Mac.", "No se ha enviado nada; se queda en este Mac.");
    match result {
        Ok(answer) if matches!(answer["state"].as_str(), Some("sent" | "pending")) => message(&answer),
        Ok(answer) => format!("{} {unsent} {}", message(&answer), later(id)),
        Err(error) => format!("{error}. {unsent} {}", later(id)),
    }
}

pub fn later(id: &str) -> String {
    trf!("To send it later: alondra report send {id}", "Para enviarlo luego: alondra report send {id}")
}

pub fn question(repo: &str) -> String {
    trf!("Send it to {repo}? Anyone who sees its issue will be able to read it.", "¿Lo envío a {repo}? Cualquiera que vea el issue podrá leerlo.")
}

/// The screen's lines, and which of them is the selected file's row.
pub fn lines(report: &CrashReport, width: usize) -> (Vec<Line<'static>>, Option<usize>) {
    let mut lines = vec![];
    let wrap = |lines: &mut Vec<Line<'static>>, text: String, style: Style| {
        lines.extend(super::md::wrap(vec![Span::styled(text, style)], width, vec![], vec![]));
    };
    let built = match &report.stage {
        Stage::Building => {
            wrap(&mut lines, tr!("Collecting the report: the logs, the tasks and the nodes of the last hours…", "Recogiendo el informe: los logs, las tareas y los nodos de las últimas horas…").into(), Style::new());
            wrap(&mut lines, tr!("Nothing leaves this Mac until you say so.", "No sale nada de este Mac hasta que lo digas.").into(), Style::new().fg(DIM));
            return (lines, None);
        }
        Stage::Done(message) | Stage::Failed(message) => {
            wrap(&mut lines, message.clone(), Style::new());
            return (lines, None);
        }
        Stage::Ready(built) | Stage::Sending(built) => built,
    };
    let (id, size, hours) = (&built.id, crate::report::size(built.compressed as usize), built.hours);
    wrap(&mut lines, trf!("Crash report {id} · last {hours} hours · {size} compressed", "Informe de fallo {id} · últimas {hours} horas · {size} comprimido"), Style::new().add_modifier(Modifier::BOLD));
    let dir = &built.dir;
    wrap(&mut lines, trf!("It is in {dir}; nothing has left this Mac.", "Está en {dir}; no ha salido nada de este Mac."), Style::new().fg(Color::Gray));
    lines.push(Line::default());
    let what = match &report.description {
        Some(text) => {
            let text = text.trim();
            trf!("“{text}”", "«{text}»")
        }
        None => tr!("(nothing written)", "(no escribiste nada)").into(),
    };
    wrap(&mut lines, format!("{} {what}", tr!("What happened?", "¿Qué pasó?")), Style::new());
    wrap(&mut lines, format!("{} {}", tr!("Kind of failure:", "Tipo de fallo:"), kind_label(&report.failure)), Style::new());
    lines.push(Line::default());
    lines.push(Line::from(tr!("FILES · Enter shows one whole", "FICHEROS · Enter enseña uno entero").bold().fg(ACCENT)));
    let openable = built.openable();
    let mut selected = None;
    for (name, size) in &built.files {
        let index = openable.iter().position(|open| open == name);
        let on = index == Some(report.sel);
        let bar = Span::styled(if on { "▌ " } else { "  " }, Style::new().fg(ACCENT));
        let (size, note) = match size {
            Ok(bytes) => (crate::report::size(*bytes as usize), String::new()),
            Err(_) if name == "manifest.json" => (String::new(), String::new()),
            Err(why) => ("—".to_owned(), format!("  {why}")),
        };
        let style = if index.is_none() { Style::new().fg(DIM) } else if on { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
        if on {
            selected = Some(lines.len());
        }
        lines.push(Line::from(vec![bar, Span::styled(format!("{name:<22}{size:>9}"), style), Span::styled(note, Style::new().fg(DIM))]));
    }
    if !built.left_out.is_empty() {
        lines.push(Line::default());
        wrap(&mut lines, format!("{} {}", tr!("Left out:", "Fuera:"), built.left_out.join(", ")), Style::new().fg(Color::Gray));
    }
    lines.push(Line::default());
    if built.passed {
        wrap(&mut lines, tr!("The final check found nothing private.", "La comprobación final no encontró nada privado.").into(), Style::new().fg(Color::Green));
        wrap(&mut lines, tr!(
            "This is exactly what is sent. Anyone with the link of its issue will be able to read it, also what you wrote in “What happened?”, which goes as you wrote it.",
            "Esto es exactamente lo que se envía. Cualquiera con el enlace del issue podrá leerlo, también lo que escribiste en «¿Qué pasó?», que va tal cual."
        ).into(), Style::new());
    } else {
        wrap(&mut lines, tr!("The report cannot leave this Mac; the final check found:", "El informe no puede salir de este Mac; la comprobación final encontró:").into(), Style::new().fg(Color::Red));
        for hit in &built.hits {
            wrap(&mut lines, format!("  {hit}"), Style::new().fg(Color::Red));
        }
    }
    lines.push(Line::default());
    match (&report.stage, &built.repo) {
        (Stage::Sending(_), Some(repo)) => wrap(&mut lines, trf!("Sending it to {repo}…", "Enviándolo a {repo}…"), Style::new().fg(ACCENT)),
        (_, Some(repo)) if built.passed => wrap(&mut lines, question(repo), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
        (_, None) if built.passed => wrap(&mut lines, trf!(
            "This Mac knows no report repository, so it stays here. To save it as a file: alondra report save {id}",
            "Este Mac no conoce ningún repositorio de informes, así que se queda aquí. Para guardarlo como fichero: alondra report save {id}"
        ), Style::new()),
        _ => {}
    }
    (lines, selected)
}

impl CrashReport {
    pub fn built(&self) -> Option<&Built> {
        match &self.stage {
            Stage::Ready(built) => Some(built),
            _ => None,
        }
    }

    pub fn step(&mut self, motion: Motion) {
        if let Some(viewing) = self.viewing.as_mut() {
            let last = match &viewing.text {
                Some(Ok(lines)) => lines.len().saturating_sub(1),
                _ => 0,
            };
            viewing.scroll = match motion {
                Motion::Down => viewing.scroll + 1,
                Motion::Up => viewing.scroll.saturating_sub(1),
                Motion::HalfDown => viewing.scroll + 20,
                Motion::HalfUp => viewing.scroll.saturating_sub(20),
                Motion::PageDown => viewing.scroll + 40,
                Motion::PageUp => viewing.scroll.saturating_sub(40),
                Motion::Top => 0,
                Motion::Bottom => last,
            }
            .min(last);
            return;
        }
        let last = self.built().map_or(0, |built| built.openable().len().saturating_sub(1));
        self.sel = match motion {
            Motion::Down => self.sel + 1,
            Motion::Up => self.sel.saturating_sub(1),
            Motion::Top => 0,
            Motion::Bottom => last,
            _ => self.sel,
        }
        .min(last);
    }
}

impl App {
    /// Asks “What happened?” for a report that goes as `failure`.
    pub(super) fn open_crash_report(&mut self, failure: &str) {
        let failure = Kind::from_word(failure).unwrap_or(Kind::Other).word().to_owned();
        self.compose(Target::ReportDescription { failure }, "");
    }

    /// The user wrote what happened, or nothing: the daemon builds the report.
    pub(super) fn build_crash_report(&mut self, failure: String, description: String) {
        let description = Some(description).filter(|text| !text.trim().is_empty());
        let body = json!({"failure": failure, "description": description, "lang": lang()});
        self.crash_report = Some(CrashReport { failure, description, stage: Stage::Building, sel: 0, scroll: 0, viewing: None });
        self.spawn(move |tx, target| {
            let result = Client::for_target(target, BUILD).post("/reports", &body).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Report(Answer::Built(result)));
        });
    }

    pub(super) fn crash_report_answered(&mut self, answer: Answer) {
        let Some(report) = self.crash_report.as_mut() else {
            if let Answer::Sent { id, result } = answer {
                self.flash(sent_message(&id, result));
            }
            return;
        };
        match answer {
            Answer::Built(result) => {
                if !matches!(report.stage, Stage::Building) {
                    return;
                }
                report.stage = match result.as_ref().map(Built::of) {
                    Ok(Some(built)) => Stage::Ready(built),
                    Ok(None) => Stage::Failed(tr!("Alondra on this Mac answered with something that is not a report.", "Alondra en este Mac respondió algo que no es un informe.").into()),
                    Err(error) => Stage::Failed(trf!("The report could not be built: {error}", "No se pudo crear el informe: {error}")),
                };
            }
            Answer::File { id, name, result } => {
                let current = report.built().is_some_and(|built| built.id == id);
                let Some(viewing) = report.viewing.as_mut().filter(|viewing| viewing.name == name && current) else { return };
                viewing.text = Some(match result {
                    Ok(answer) => answer["text"].as_str().map(|text| text.lines().map(str::to_owned).collect())
                        .ok_or_else(|| tr!("Alondra on this Mac did not send the file.", "Alondra en este Mac no envió el fichero.").to_owned()),
                    Err(error) => Err(error),
                });
            }
            Answer::Sent { id, result } => {
                if !matches!(&report.stage, Stage::Sending(built) if built.id == id) {
                    return;
                }
                report.stage = Stage::Done(sent_message(&id, result));
            }
        }
    }

    /// Enter on a file: shows it whole, as the daemon reads it from the report.
    pub(super) fn open_report_file(&mut self) {
        let Some(report) = self.crash_report.as_mut() else { return };
        let Some((id, name)) = report.built().and_then(|built| Some((built.id.clone(), (*built.openable().get(report.sel)?).to_owned()))) else { return };
        report.viewing = Some(Viewing { name: name.clone(), text: None, scroll: 0 });
        self.spawn(move |tx, target| {
            let result = Client::for_target(target, Duration::from_secs(60)).get::<Value>(&format!("/reports/{id}/files/{name}")).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Report(Answer::File { id, name, result }));
        });
    }

    /// “Yes, send it”.
    pub(super) fn send_crash_report(&mut self) {
        let Some(report) = self.crash_report.as_mut() else { return };
        let Some(built) = report.built().filter(|built| built.sendable()).cloned() else { return };
        let (id, body) = (built.id.clone(), json!({"failure": report.failure, "lang": lang()}));
        report.stage = Stage::Sending(built);
        self.spawn(move |tx, target| {
            let result = Client::for_target(target, SEND).post(&format!("/reports/{id}/send"), &body).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Report(Answer::Sent { id, result }));
        });
    }

    /// “No, keep it on this Mac”.
    pub(super) fn keep_crash_report(&mut self) {
        let Some(report) = self.crash_report.as_mut() else { return };
        let Some(id) = report.built().map(|built| built.id.clone()) else { return };
        report.stage = Stage::Done(format!("{} {}", tr!("It stays on this Mac.", "Se queda en este Mac."), later(&id)));
    }

    /// Back: from a file to the report, and from the report out. A report built and not sent stays
    /// on the Mac, and the screen says how to send it later.
    pub(super) fn close_crash_report(&mut self) {
        let Some(report) = self.crash_report.as_mut() else { return };
        if report.viewing.take().is_some() {
            return;
        }
        let kept = match &report.stage {
            Stage::Ready(built) => Some(format!("{} {}", tr!("The report stays on this Mac.", "El informe se queda en este Mac."), later(&built.id))),
            _ => None,
        };
        self.crash_report = None;
        if let Some(kept) = kept {
            self.flash(kept);
        }
    }
}
