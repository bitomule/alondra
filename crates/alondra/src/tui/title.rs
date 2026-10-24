use std::io::{self, Write};

use crossterm::terminal::SetTitle;

/// The terminal tab's name: the cards waiting for the user on every Mac, `None` for a Mac
/// that does not answer. Offline only when none answers.
pub fn text(attention: &[Option<usize>]) -> String {
    let known: Vec<usize> = attention.iter().flatten().copied().collect();
    if known.is_empty() && !attention.is_empty() {
        return tr!("Alondra · offline", "Alondra · sin conexión").to_owned();
    }
    match known.iter().sum::<usize>() {
        0 => "Alondra".to_owned(),
        waiting => format!("({waiting}) Alondra"),
    }
}

/// Writes the title only when it changes, so redrawing never floods the terminal.
#[derive(Default)]
pub struct TerminalTitle {
    written: Option<String>,
}

impl TerminalTitle {
    pub fn show(&mut self, out: &mut impl Write, title: String) -> io::Result<()> {
        if self.written.as_ref() == Some(&title) {
            return Ok(());
        }
        crossterm::queue!(out, SetTitle(&title))?;
        out.flush()?;
        self.written = Some(title);
        Ok(())
    }

    /// A tool that ran in the terminal may have renamed the tab.
    pub fn forget(&mut self) {
        self.written = None;
    }
}

/// Leaves the tab to the terminal's own name: the title from before Alondra was another
/// program's, usually a Claude session already closed.
pub fn clear(out: &mut impl Write) {
    let _ = crossterm::queue!(out, SetTitle(""));
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Lang, set_for_this_thread};

    #[test]
    fn the_title_counts_what_waits_on_every_mac() {
        set_for_this_thread(Lang::Es);
        assert_eq!(text(&[Some(0)]), "Alondra");
        assert_eq!(text(&[Some(3)]), "(3) Alondra");
        assert_eq!(text(&[Some(2), Some(1)]), "(3) Alondra");
        assert_eq!(text(&[None, Some(1)]), "(1) Alondra");
        assert_eq!(text(&[None, None]), "Alondra · sin conexión");
        set_for_this_thread(Lang::En);
        assert_eq!(text(&[None]), "Alondra · offline");
    }

    #[test]
    fn the_title_is_written_only_when_it_changes() {
        let mut title = TerminalTitle::default();
        let mut out = Vec::new();
        title.show(&mut out, "(3) Alondra".into()).unwrap();
        assert_eq!(out, b"\x1b]0;(3) Alondra\x07");
        out.clear();
        title.show(&mut out, "(3) Alondra".into()).unwrap();
        assert!(out.is_empty());
        title.show(&mut out, "Alondra".into()).unwrap();
        assert_eq!(out, b"\x1b]0;Alondra\x07");
        out.clear();
        title.forget();
        title.show(&mut out, "Alondra".into()).unwrap();
        assert_eq!(out, b"\x1b]0;Alondra\x07");
    }

    #[test]
    fn leaving_clears_the_title() {
        let mut out = Vec::new();
        clear(&mut out);
        assert_eq!(out, b"\x1b]0;\x07");
    }
}
