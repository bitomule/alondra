//! Connect on the card of the tasks that wait for an MCP server. On the daemon's Mac the sign-in
//! opens in its browser, which comes back to Claude on the same Mac by itself. Elsewhere the
//! browser opens here and comes back to a page that does not load: the user pastes its address,
//! which goes to the daemon and is never kept here, not even as a draft.
use std::time::Duration;

use serde_json::{Value, json};

use super::{App, Msg, Target};
use crate::client::Client;

/// What the daemon answered to Connect.
#[derive(Debug, PartialEq, Eq)]
pub enum Opened {
    Browser { url: String, returns_to_claude: bool },
    Connected,
}

pub fn opened(body: &Value) -> Option<Opened> {
    if body["connected"].as_bool() == Some(true) {
        return Some(Opened::Connected);
    }
    let url = body["url"].as_str().filter(|url| ["https://", "http://localhost:", "http://127.0.0.1:"].iter().any(|scheme| url.starts_with(scheme)))?;
    Some(Opened::Browser { url: url.to_owned(), returns_to_claude: body["returns_to_claude"].as_bool() == Some(true) })
}

pub fn path(card: &str, step: &str) -> String {
    format!("/mcp-sign-ins/{}/{step}", crate::mcp::urlencode(card))
}

impl App {
    pub(super) fn mcp_connect(&mut self, card: &str, server: &str) {
        let path = path(card, "connect");
        #[cfg(test)]
        self.posted.borrow_mut().push((path.clone(), json!({})));
        self.flash(trf!("Opening the sign-in to {server}…", "Abriendo el inicio de sesión en {server}…"));
        let (card, server) = (card.to_owned(), server.to_owned());
        self.spawn(move |tx, target| {
            let result = Client::for_target(target, Duration::from_secs(45))
                .post(&path, &json!({}))
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(Msg::McpOpened { card, server, result });
        });
    }

    pub(super) fn mcp_reconnect(&mut self, card: &str) {
        self.op(None, path(card, "reconnect"), json!({}),
            tr!("Its nodes read their sign-in again", "Sus nodos vuelven a leer su sesión").into());
    }

    pub(super) fn mcp_opened(&mut self, card: String, server: String, result: Result<Value, String>) {
        let body = match result {
            Ok(body) => body,
            Err(error) => return self.fail(trf!("Could not open the sign-in to {server}: {error}", "No se pudo abrir el inicio de sesión en {server}: {error}")),
        };
        match opened(&body) {
            None => self.fail(trf!("The daemon did not give a sign-in page for {server}", "El daemon no dio una página para iniciar sesión en {server}")),
            Some(Opened::Connected) => self.flash(trf!("{server} is connected: the waiting tasks go on", "{server} está conectado: las tareas que esperan siguen")),
            Some(Opened::Browser { url, returns_to_claude }) => {
                super::open_with_mac(&url);
                if !returns_to_claude {
                    self.flash(trf!(
                        "Connect {server} on claude.ai in the browser, then press Reconnect",
                        "Conecta {server} en claude.ai en el navegador y luego pulsa Reconectar"
                    ));
                } else if self.remote_mac().is_none() {
                    self.flash(trf!(
                        "Sign in to {server} in the browser: the waiting tasks go on by themselves",
                        "Inicia sesión en {server} en el navegador: las tareas que esperan siguen solas"
                    ));
                } else {
                    self.flash(tr!("The sign-in page opened in this computer's browser", "La página para iniciar sesión se abrió en el navegador de este equipo"));
                    self.compose(Target::McpReturn { card, server, url }, "");
                }
            }
        }
    }

    pub(super) fn mcp_give_back(&mut self, card: &str, server: &str, address: String) {
        let ok = trf!("Signed in to {server}: the waiting tasks go on", "Sesión iniciada en {server}: las tareas que esperan siguen");
        self.op(None, path(card, "return"), json!({"address": address}), ok);
    }
}
