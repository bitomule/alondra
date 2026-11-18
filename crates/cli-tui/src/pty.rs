use std::io::Write;
use std::os::fd::RawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result, bail};

const TAIL_BYTES: usize = 2048;

/// Raw input while a tool runs over the TUI, which keeps its alternate screen so the plain terminal
/// never shows in between.
struct RawMode;

impl RawMode {
    fn enter() -> Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(RawMode)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        write_modes(RESET_INPUT_MODES);
    }
}

/// Mouse, focus, paste and keyboard-protocol modes a full-screen tool may leave switched on.
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

/// Drops the tool's switches in and out of the alternate screen, so over the TUI it draws
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

/// Copies the child's screen to ours and our keys to it until it exits, and returns the last bytes
/// it printed.
fn relay(master: &Fd, mut ws: libc::winsize, child: &mut Child) -> Result<Vec<u8>> {
    let mut screen = AltScreenFilter::default();
    let mut stdout = std::io::stdout();
    let mut buf = [0u8; 16 * 1024];
    let mut tail: Vec<u8> = Vec::new();
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
            stdout.write_all(&screen.feed(&buf[..got as usize]))?;
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
            unsafe { libc::write(master.0, input.as_ptr().cast(), input.len()) };
        }
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
    }
    Ok(tail)
}

/// Runs a full-screen tool (tuicr) over the TUI, and returns what it printed on stdout when it
/// quit.
pub fn run_tool(program: &str, args: &[&str], dir: &std::path::Path) -> Result<String> {
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

    let raw = RawMode::enter()?;
    let tail = relay(&master, ws, &mut child)?;
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

/// The last line of visible text a failing tool printed, without escape sequences.
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
    fn last_words_skip_escape_sequences() {
        let out = b"\x1b[2J\x1b[1;31mError:\x1b[0m no review for tui-poli\r\n\x1b[?25h";
        assert_eq!(last_words(out).as_deref(), Some("Error: no review for tui-poli"));
        assert_eq!(last_words(b"\x1b[2J"), None);
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
