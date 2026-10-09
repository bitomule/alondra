use std::time::Duration;

use serde_json::{Value, json};

use super::ui::{Cancelled, Progress, Ui};

pub const LIMIT: Duration = Duration::from_secs(5 * 60);
pub const HINT_AFTER: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub id: String,
    pub name: String,
    pub code: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Decided {
    Done,
    Expired,
}

/// The daemon's pending pairing requests, and the decision on one.
pub trait Pairings {
    fn pending(&self) -> Vec<Request>;
    fn decide(&self, id: &str, allow: bool) -> Decided;
}

/// Time and the Enter key, so the wait runs in tests without sleeping.
pub trait Clock {
    fn elapsed(&self) -> Duration;
    fn pause(&self, how_long: Duration);
    /// Whether the person pressed Enter to finish.
    fn finished(&self) -> bool;
}

/// `482913` reads better as `482 913`.
fn spaced(code: &str) -> String {
    if code.len() == 6 && code.is_ascii() { format!("{} {}", &code[..3], &code[3..]) } else { code.to_string() }
}

fn waiting(ui: &mut dyn Ui) -> Box<dyn Progress> {
    ui.progress(tr!("Waiting for your iPhone… open Alondra on it. Press Enter to finish.",
        "Esperando a tu iPhone… abre Alondra en él. Pulsa Enter para terminar."))
}

/// The end of the wizard (§3.3): shows each pairing request on its own, with its code, until
/// Enter, five minutes, or nobody left to ask. Allowing needs a `y`; no is the default. Returns
/// how many devices were allowed.
pub fn wait(pairings: &dyn Pairings, ui: &mut dyn Ui, clock: &dyn Clock) -> Result<usize, Cancelled> {
    let (mut decided, mut allowed, mut hinted) = (Vec::<String>::new(), 0, false);
    let mut spinner = Some(waiting(ui));
    while !clock.finished() && clock.elapsed() < LIMIT {
        let next = pairings.pending().into_iter().find(|r| !decided.contains(&r.id));
        let Some(request) = next else {
            if !hinted && allowed == 0 && decided.is_empty() && clock.elapsed() >= HINT_AFTER {
                hinted = true;
                if let Some(s) = spinner.take() {
                    s.finish();
                }
                ui.note(&[tr!("Not showing up? Your network may block discovery — use Add by address in the app.",
                    "¿No aparece? Puede que tu red bloquee la búsqueda: usa Añadir por dirección en la app.").into()]);
                spinner = Some(waiting(ui));
            }
            clock.pause(POLL);
            continue;
        };
        if let Some(s) = spinner.take() {
            s.finish();
        }
        let (name, code) = (request.name.as_str(), spaced(&request.code));
        let question = trf!("Allow “{name}”? Code {code} — allow only if it shows the same code.",
            "¿Permites «{name}»? Código {code}: permítelo solo si muestra el mismo código.");
        let Some(allow) = ui.confirm(&question, false)? else { return Ok(allowed) };
        decided.push(request.id.clone());
        match pairings.decide(&request.id, allow) {
            Decided::Expired => ui.note(&[tr!("That request expired — ask again from the device.",
                "Esa petición caducó: vuelve a pedirlo desde el dispositivo.").into()]),
            Decided::Done if allow => allowed += 1,
            Decided::Done => {}
        }
        spinner = Some(waiting(ui));
    }
    if let Some(s) = spinner {
        s.finish();
    }
    Ok(allowed)
}

pub struct Daemon(pub crate::client::Client);

impl Pairings for Daemon {
    fn pending(&self) -> Vec<Request> {
        let Ok(list) = self.0.get::<Value>("/devices/pending") else { return vec![] };
        let text = |r: &Value, k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        list.get("pending").and_then(Value::as_array).into_iter().flatten()
            .map(|r| Request { id: text(r, "id"), name: text(r, "name"), code: text(r, "code") })
            .filter(|r| !r.id.is_empty())
            .collect()
    }

    fn decide(&self, id: &str, allow: bool) -> Decided {
        let verb = if allow { "approve" } else { "deny" };
        let path = format!("/devices/pending/{}/{verb}", crate::mcp::urlencode(id));
        match self.0.exchange("POST", &path, Some(&json!({}))) {
            Ok((410, _)) => Decided::Expired,
            _ => Decided::Done,
        }
    }
}

/// The real clock; Enter is read only when a whole line is waiting, so the questions in between
/// keep their keys.
pub struct Terminal(pub std::time::Instant);

impl Clock for Terminal {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }

    fn pause(&self, how_long: Duration) {
        std::thread::sleep(how_long);
    }

    fn finished(&self) -> bool {
        let mut fd = libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 };
        if unsafe { libc::poll(&mut fd, 1, 0) } <= 0 || fd.revents & libc::POLLIN == 0 {
            return false;
        }
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        true
    }
}
