use std::io::Write;
use std::os::fd::RawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result, bail};

const LEFT_ARROWS: [&[u8]; 2] = [b"\x1b[D", b"\x1bOD"];
const TAIL_BYTES: usize = 2048;

/// Where the attach runs: over the TUI, which keeps its alternate screen so the plain terminal
/// never shows in between, or straight from a shell.
#[derive(Clone, Copy, PartialEq)]
pub enum Over {
    Tui,
    Shell,
}

struct RawMode(Over);

impl RawMode {
    fn enter(over: Over) -> Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(RawMode(over))
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        write_modes(if self.0 == Over::Tui { RESET_INPUT_MODES } else { RESET_MODES });
    }
}

/// Mouse, focus, paste and keyboard-protocol modes a Claude session may leave switched on.
const RESET_INPUT_MODES: &[u8] =
    b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?1004l\x1b[?2004l\x1b[<99u\x1b[=0;1u\x1b[>4;0m";
const RESET_MODES: &[u8] =
    b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?1004l\x1b[?2004l\x1b[<99u\x1b[=0;1u\x1b[>4;0m\x1b[?1049l\x1b[?25h";

fn write_modes(modes: &[u8]) {
    let mut out = std::io::stdout();
    let _ = out.write_all(modes);
    let _ = out.flush();
}

pub fn reset_terminal_modes() {
    write_modes(RESET_MODES);
}

const ALT_SCREEN: [&[u8]; 6] = [b"\x1b[?1049h", b"\x1b[?1049l", b"\x1b[?1047h", b"\x1b[?1047l", b"\x1b[?47h", b"\x1b[?47l"];

/// Drops the session's switches in and out of the alternate screen, so over the TUI it draws
/// on the screen the TUI already owns and leaving it never uncovers the plain terminal. A
/// sequence split across two reads is held until the next one.
#[derive(Default)]
struct AltScreenFilter {
    carry: Vec<u8>,
}

impl AltScreenFilter {
    fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut data = std::mem::take(&mut self.carry);
        data.extend_from_slice(bytes);
        let mut out = Vec::with_capacity(data.len());
        let mut i = 0;
        while i < data.len() {
            if data[i] == 0x1b {
                let rest = &data[i..];
                if let Some(seq) = ALT_SCREEN.iter().find(|s| rest.starts_with(s)) {
                    i += seq.len();
                    continue;
                }
                if ALT_SCREEN.iter().any(|s| s.starts_with(rest)) {
                    self.carry = rest.to_vec();
                    break;
                }
            }
            out.push(data[i]);
            i += 1;
        }
        out
    }
}

struct Fd(RawFd);

impl Drop for Fd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

fn window_size() -> libc::winsize {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
    if ws.ws_row == 0 || ws.ws_col == 0 {
        ws.ws_row = 50;
        ws.ws_col = 200;
    }
    ws
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Esc {
    #[default]
    None,
    Start,
    Csi,
    Ss3,
    Str,
    StrEnd,
}

/// Characters typed on the node's prompt line since it was last empty. Escape
/// sequences (arrows, function keys, and the terminal's own replies to queries)
/// do not count; Enter, Ctrl+U and Ctrl+C empty it.
#[derive(Default)]
struct LineTracker {
    typed: usize,
    esc: Esc,
}

impl LineTracker {
    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.esc = match self.esc {
                Esc::Start => match b {
                    b'[' => Esc::Csi,
                    b'O' => Esc::Ss3,
                    b'P' | b']' | b'_' | b'^' | b'X' => Esc::Str,
                    _ => Esc::None,
                },
                Esc::Csi if (0x40..=0x7e).contains(&b) => Esc::None,
                Esc::Csi => Esc::Csi,
                Esc::Ss3 => Esc::None,
                Esc::Str if b == 0x07 => Esc::None,
                Esc::Str if b == 0x1b => Esc::StrEnd,
                Esc::Str => Esc::Str,
                Esc::StrEnd if b == b'\\' => Esc::None,
                Esc::StrEnd => Esc::Str,
                Esc::None => {
                    match b {
                        0x1b => {
                            self.esc = Esc::Start;
                            continue;
                        }
                        b'\r' | b'\n' | 0x15 | 0x03 => self.typed = 0,
                        0x7f | 0x08 => self.typed = self.typed.saturating_sub(1),
                        0x80..=0xbf => {}
                        0x20.. => self.typed += 1,
                        _ => {}
                    }
                    Esc::None
                }
            };
        }
    }
}

fn open_pty() -> Result<(Fd, RawFd, libc::winsize)> {
    let mut ws = window_size();
    let (mut master, mut slave): (RawFd, RawFd) = (-1, -1);
    let r = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut ws) };
    if r != 0 {
        bail!(tr!("could not open a pseudoterminal", "no pude abrir un pseudoterminal"));
    }
    Ok((Fd(master), slave, ws))
}

/// Makes the pseudoterminal the child's controlling terminal, so what it draws on `/dev/tty`
/// comes through it too.
fn in_pty(cmd: &mut Command, slave: RawFd, stdout: Stdio) {
    unsafe {
        cmd.stdin(Stdio::from_raw_fd_owned(libc::dup(slave)))
            .stdout(stdout)
            .stderr(Stdio::from_raw_fd_owned(libc::dup(slave)))
            .pre_exec(|| {
                libc::setsid();
                libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                Ok(())
            });
    }
}

/// Copies the child's screen to ours and our keys to it until it exits, or until ← on an empty
/// prompt when `detach_on_left`. Returns whether it was left that way, and the last bytes it
/// printed.
fn relay(master: &Fd, mut ws: libc::winsize, child: &mut Child, over: Over, detach_on_left: bool) -> Result<(bool, Vec<u8>)> {
    let mut screen = (over == Over::Tui).then(AltScreenFilter::default);
    let mut stdout = std::io::stdout();
    let mut line = LineTracker::default();
    let mut buf = [0u8; 16 * 1024];
    let mut tail: Vec<u8> = Vec::new();
    let mut log = std::env::var("ALONDRA_ATTACH_LOG").ok().and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
    loop {
        let current = window_size();
        if current.ws_row != ws.ws_row || current.ws_col != ws.ws_col {
            ws = current;
            unsafe { libc::ioctl(master.0, libc::TIOCSWINSZ, &ws) };
        }
        let mut fds = [
            libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: master.0, events: libc::POLLIN, revents: 0 },
        ];
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, 200) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            let got = unsafe { libc::read(master.0, buf.as_mut_ptr().cast(), buf.len()) };
            if got <= 0 {
                break;
            }
            match screen.as_mut() {
                Some(filter) => stdout.write_all(&filter.feed(&buf[..got as usize]))?,
                None => stdout.write_all(&buf[..got as usize])?,
            }
            stdout.flush()?;
            tail.extend_from_slice(&buf[..got as usize]);
            if tail.len() > TAIL_BYTES {
                tail.drain(..tail.len() - TAIL_BYTES);
            }
        }
        if fds[0].revents & libc::POLLIN != 0 {
            let got = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
            if got <= 0 {
                break;
            }
            let input = &buf[..got as usize];
            if let Some(f) = log.as_mut() {
                let _ = writeln!(f, "typed={} input={:?}", line.typed, input);
            }
            if detach_on_left && line.typed == 0 && LEFT_ARROWS.contains(&input) {
                return Ok((true, tail));
            }
            line.feed(input);
            unsafe { libc::write(master.0, input.as_ptr().cast(), input.len()) };
        }
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
    }
    Ok((false, tail))
}

pub fn attach(short_id: &str, over: Over) -> Result<()> {
    let (master, slave, ws) = open_pty()?;
    let mut cmd = Command::new("claude");
    cmd.arg("attach").arg(short_id).current_dir("/tmp");
    in_pty(&mut cmd, slave, unsafe { Stdio::from_raw_fd_owned(libc::dup(slave)) });
    let mut child = cmd.spawn().context(tr!("could not launch claude attach", "no pude lanzar claude attach"))?;
    unsafe { libc::close(slave) };

    let raw = RawMode::enter(over)?;
    let (left_to_alondra, tail) = relay(&master, ws, &mut child, over, true)?;
    drop(raw);
    if left_to_alondra || child.try_wait().ok().flatten().is_none() {
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    }
    match child.wait() {
        Ok(st) if !left_to_alondra && !st.success() => match last_words(&tail) {
            Some(words) => bail!("{words}"),
            None => bail!(trf!("claude attach ended with {st}", "claude attach terminó con {st}")),
        },
        _ => Ok(()),
    }
}

/// Runs a full-screen tool (tuicr) over the TUI the way `attach` runs a session, and returns
/// what it printed on stdout when it quit.
pub fn run_tool(program: &str, args: &[&str], dir: &std::path::Path, over: Over) -> Result<String> {
    let (master, slave, ws) = open_pty()?;
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(dir);
    in_pty(&mut cmd, slave, Stdio::piped());
    let mut child = cmd.spawn().with_context(|| trf!("could not launch {program}", "no pude lanzar {program}"))?;
    unsafe { libc::close(slave) };
    let mut out = child.stdout.take().context("no stdout")?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut out, &mut text);
        text
    });

    let raw = RawMode::enter(over)?;
    let (_, tail) = relay(&master, ws, &mut child, over, false)?;
    drop(raw);
    let status = child.wait()?;
    let printed = reader.join().unwrap_or_default();
    if !status.success() {
        match last_words(&tail) {
            Some(words) => bail!("{words}"),
            None => bail!(trf!("{program} ended with {status}", "{program} terminó con {status}")),
        }
    }
    Ok(printed)
}

/// The last line of visible text a failing `claude attach` printed, without escape sequences.
fn last_words(output: &[u8]) -> Option<String> {
    let mut plain = String::new();
    let mut esc = false;
    for c in String::from_utf8_lossy(output).chars() {
        match (esc, c) {
            (false, '\x1b') => esc = true,
            (true, c) if c.is_ascii_alphabetic() || c == '\x07' => esc = false,
            (true, _) => {}
            (false, c) if c == '\n' || !c.is_control() => plain.push(c),
            _ => {}
        }
    }
    plain.lines().map(str::trim).rfind(|l| !l.is_empty()).map(|l| l.chars().take(160).collect())
}

trait FromRawFdOwned {
    unsafe fn from_raw_fd_owned(fd: RawFd) -> Stdio;
}

impl FromRawFdOwned for Stdio {
    unsafe fn from_raw_fd_owned(fd: RawFd) -> Stdio {
        use std::os::fd::FromRawFd;
        unsafe { Stdio::from_raw_fd(fd) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn over_the_tui_the_session_never_leaves_the_alternate_screen() {
        let mut f = AltScreenFilter::default();
        assert_eq!(f.feed(b"\x1b[?1049h\x1b[2Jhello"), b"\x1b[2Jhello");
        assert_eq!(f.feed(b"bye\x1b[?10"), b"bye", "a switch split across reads waits for the rest");
        assert_eq!(f.feed(b"49l\x1b[?25h"), b"\x1b[?25h");
        assert_eq!(f.feed(b"\x1b[1mbold\x1b[0m\x1b[?47l"), b"\x1b[1mbold\x1b[0m");
    }

    #[test]
    fn empty_line_after_enter_and_escape_sequences_do_not_count() {
        let mut t = LineTracker::default();
        t.feed(b"hola");
        assert_eq!(t.typed, 4);
        t.feed(b"\x1b[D\x1b[C");
        assert_eq!(t.typed, 4);
        t.feed(b"\x7f\x7f");
        assert_eq!(t.typed, 2);
        t.feed(b"\r");
        assert_eq!(t.typed, 0);
        t.feed("ñ".as_bytes());
        assert_eq!(t.typed, 1);
    }

    #[test]
    fn last_words_skip_escape_sequences() {
        let out = b"\x1b[2J\x1b[1;31mError:\x1b[0m no session tui-poli\r\n\x1b[?25h";
        assert_eq!(last_words(out).as_deref(), Some("Error: no session tui-poli"));
        assert_eq!(last_words(b"\x1b[2J"), None);
    }

    #[test]
    fn terminal_replies_are_not_typing() {
        let mut t = LineTracker::default();
        t.feed(b"\x1bP>|tmux 3.5a\x1b\\\x1b[?1;2;4c");
        t.feed(b"\x1b]11;rgb:0000/0000/0000\x07");
        assert_eq!(t.typed, 0);
    }
}

#[cfg(test)]
mod reset_tests {
    use super::RESET_MODES;

    #[test]
    fn reset_clears_every_keyboard_mode_claude_code_turns_on() {
        let reset = String::from_utf8_lossy(RESET_MODES);
        for off in ["\x1b[>4;0m", "\x1b[<99u", "\x1b[?1003l", "\x1b[?1006l", "\x1b[?1004l", "\x1b[?2004l"] {
            assert!(reset.contains(off), "missing {off:?}");
        }
    }
}
