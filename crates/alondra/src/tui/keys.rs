use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Motion {
    Down,
    Up,
    Top,
    Bottom,
    HalfDown,
    HalfUp,
    PageDown,
    PageUp,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    Move(Motion),
    Left,
    Right,
    Expand,
    Back,
    Pending,
    Other(KeyEvent),
}

/// Normal-mode keys, vim style. Text fields never go through here: they get the raw key.
#[derive(Default)]
pub struct Keymap {
    g: bool,
}

impl Keymap {
    pub fn read(&mut self, k: KeyEvent) -> Cmd {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if std::mem::take(&mut self.g) && k.code == KeyCode::Char('g') && !ctrl {
            return Cmd::Move(Motion::Top);
        }
        match k.code {
            KeyCode::Char('g') if !ctrl => {
                self.g = true;
                Cmd::Pending
            }
            KeyCode::Char('d') if ctrl => Cmd::Move(Motion::HalfDown),
            KeyCode::Char('u') if ctrl => Cmd::Move(Motion::HalfUp),
            KeyCode::Char('f') if ctrl => Cmd::Move(Motion::PageDown),
            KeyCode::Char('b') if ctrl => Cmd::Move(Motion::PageUp),
            KeyCode::Char('e') if ctrl => Cmd::Move(Motion::Down),
            KeyCode::Char('y') if ctrl => Cmd::Move(Motion::Up),
            _ if ctrl => Cmd::Other(k),
            KeyCode::Char('j') | KeyCode::Down => Cmd::Move(Motion::Down),
            KeyCode::Char('k') | KeyCode::Up => Cmd::Move(Motion::Up),
            KeyCode::Char('G') | KeyCode::End => Cmd::Move(Motion::Bottom),
            KeyCode::Home => Cmd::Move(Motion::Top),
            KeyCode::PageDown | KeyCode::Char(' ') => Cmd::Move(Motion::PageDown),
            KeyCode::PageUp => Cmd::Move(Motion::PageUp),
            KeyCode::Char('h') | KeyCode::Left => Cmd::Left,
            KeyCode::Char('l') | KeyCode::Right => Cmd::Right,
            KeyCode::Char('o') => Cmd::Expand,
            KeyCode::Esc => Cmd::Back,
            _ => Cmd::Other(k),
        }
    }
}

/// Lines a motion scrolls in a view `h` rows tall; `None` for Top/Bottom.
pub fn lines(m: Motion, h: usize) -> Option<isize> {
    let half = (h / 2).max(1) as isize;
    let page = h.saturating_sub(2).max(1) as isize;
    match m {
        Motion::Down => Some(1),
        Motion::Up => Some(-1),
        Motion::HalfDown => Some(half),
        Motion::HalfUp => Some(-half),
        Motion::PageDown => Some(page),
        Motion::PageUp => Some(-page),
        Motion::Top | Motion::Bottom => None,
    }
}

pub fn scroll(pos: usize, m: Motion, h: usize, total: usize) -> usize {
    match (m, lines(m, h)) {
        (Motion::Top, _) => 0,
        (_, None) => total.saturating_sub(h),
        (_, Some(d)) => by(pos, d, h, total),
    }
}

pub fn by(pos: usize, d: isize, h: usize, total: usize) -> usize {
    (pos as isize + d).clamp(0, total.saturating_sub(h) as isize) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn gg_needs_two_presses_and_anything_else_cancels_it() {
        let mut m = Keymap::default();
        assert_eq!(m.read(key(KeyCode::Char('g'))), Cmd::Pending);
        assert_eq!(m.read(key(KeyCode::Char('g'))), Cmd::Move(Motion::Top));
        assert_eq!(m.read(key(KeyCode::Char('g'))), Cmd::Pending);
        assert_eq!(m.read(key(KeyCode::Char('j'))), Cmd::Move(Motion::Down));
        assert_eq!(m.read(key(KeyCode::Char('g'))), Cmd::Pending);
        assert_eq!(m.read(ctrl('g')), Cmd::Other(ctrl('g')));
        assert_eq!(m.read(key(KeyCode::Char('g'))), Cmd::Pending);
    }

    #[test]
    fn vim_keys_and_arrows_mean_the_same() {
        let mut m = Keymap::default();
        let pairs = [
            (key(KeyCode::Char('j')), key(KeyCode::Down)),
            (key(KeyCode::Char('k')), key(KeyCode::Up)),
            (key(KeyCode::Char('h')), key(KeyCode::Left)),
            (key(KeyCode::Char('l')), key(KeyCode::Right)),
            (key(KeyCode::Char('G')), key(KeyCode::End)),
            (ctrl('f'), key(KeyCode::PageDown)),
            (ctrl('b'), key(KeyCode::PageUp)),
        ];
        for (a, b) in pairs {
            assert_eq!(m.read(a), m.read(b), "{a:?} vs {b:?}");
        }
        assert_eq!(m.read(ctrl('d')), Cmd::Move(Motion::HalfDown));
        assert_eq!(m.read(ctrl('u')), Cmd::Move(Motion::HalfUp));
        assert_eq!(m.read(key(KeyCode::Char('o'))), Cmd::Expand);
        assert_eq!(m.read(key(KeyCode::Esc)), Cmd::Back);
        assert_eq!(m.read(key(KeyCode::Char('d'))), Cmd::Other(key(KeyCode::Char('d'))));
    }

    #[test]
    fn scrolling_stays_inside_the_content() {
        assert_eq!(scroll(0, Motion::HalfDown, 20, 100), 10);
        assert_eq!(scroll(75, Motion::PageDown, 20, 100), 80);
        assert_eq!(scroll(5, Motion::PageUp, 20, 100), 0);
        assert_eq!(scroll(40, Motion::Bottom, 20, 100), 80);
        assert_eq!(scroll(40, Motion::Top, 20, 100), 0);
        assert_eq!(scroll(0, Motion::Down, 20, 10), 0);
    }
}
