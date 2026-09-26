use std::io::Write;
use std::os::fd::RawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result, bail};

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

/// `claude attach` leaves a session on ← by exec'ing itself, same pid, into `claude agents`,
/// Claude Code's own list of background sessions. Seen from Alondra that means the node was left.
fn is_agent_view(argv: &[String]) -> bool {
    argv.iter().skip(1).any(|a| a == "agents") && !argv.iter().any(|a| a == "attach")
}

/// The argument list inside a `KERN_PROCARGS2` buffer: argc, the executable path, padding, then
/// argv.
fn procargs_argv(buf: &[u8]) -> Vec<String> {
    let Some(argc) = buf.get(..4).map(|b| i32::from_ne_bytes([b[0], b[1], b[2], b[3]])) else {
        return Vec::new();
    };
    let rest = &buf[4..];
    let rest = &rest[rest.iter().position(|&b| b == 0).unwrap_or(rest.len())..];
    let rest = &rest[rest.iter().position(|&b| b != 0).unwrap_or(rest.len())..];
    rest.split(|&b| b == 0).take(argc.max(0) as usize).map(|a| String::from_utf8_lossy(a).into_owned()).collect()
}

#[cfg(target_os = "macos")]
fn process_argv(pid: u32) -> Vec<String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) } != 0 {
        return Vec::new();
    }
    let mut buf = vec![0u8; size];
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) } != 0 {
        return Vec::new();
    }
    buf.truncate(size);
    procargs_argv(&buf)
}

#[cfg(not(target_os = "macos"))]
fn process_argv(pid: u32) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| b.split(|&c| c == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect())
        .unwrap_or_default()
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

/// Copies the child's screen to ours and our keys to it until it exits, or, when `until_agent_view`,
/// until it turns into Claude Code's agent view, whose screen is never shown. Returns whether it
/// ended that way, and the last bytes it printed.
fn relay(master: &Fd, mut ws: libc::winsize, child: &mut Child, over: Over, until_agent_view: bool) -> Result<(bool, Vec<u8>)> {
    let mut screen = (over == Over::Tui).then(AltScreenFilter::default);
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
        if until_agent_view && is_agent_view(&process_argv(child.id())) {
            return Ok((true, tail));
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
            unsafe { libc::write(master.0, input.as_ptr().cast(), input.len()) };
        }
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
    }
    Ok((false, tail))
}

pub fn attach(short_id: &str, over: Over) -> Result<()> {
    if short_id == "alondra-orchestrator" {
        bail!("{}", tr!("Alondra manages the orchestrator automatically. Open its activity in the fleet.", "Alondra gestiona el orquestador automáticamente. Abre su actividad en la flota."));
    }
    let (master, slave, ws) = open_pty()?;
    let mut cmd = Command::new("claude");
    cmd.arg("attach").arg(short_id).current_dir("/tmp");
    in_pty(&mut cmd, slave, unsafe { Stdio::from_raw_fd_owned(libc::dup(slave)) });
    let mut child = cmd.spawn().context(tr!("could not launch claude attach", "no pude lanzar claude attach"))?;
    unsafe { libc::close(slave) };

    let raw = RawMode::enter(over)?;
    let (left_to_alondra, tail) = relay(&master, ws, &mut child, over, true)?;
    drop(raw);
    drop(master);
    if left_to_alondra {
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGKILL) };
    } else if child.try_wait().ok().flatten().is_none() {
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
    fn only_the_agent_view_counts_as_leaving() {
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let cases: &[(&[&str], bool)] = &[
            (&["/x/node_modules/.bin/claude", "attach", "7ab69216"], false),
            (&["/x/claude-code/bin/claude.exe", "agents"], true),
            (&["/usr/local/bin/node", "/x/cli.js", "agents"], true),
            (&["/x/wrapper", "--flag", "/x/claude", "agents"], true),
            (&["claude", "attach", "agents"], false),
            (&["agents"], false),
            (&["tuicr", "pr", "3"], false),
            (&[], false),
        ];
        for (a, left) in cases {
            assert_eq!(is_agent_view(&argv(a)), *left, "{a:?}");
        }
    }

    #[test]
    fn procargs_buffer_yields_argv() {
        let mut buf = 2i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/x/claude.exe\0\0\0\0claude\0agents\0PATH=/bin\0HOME=/h\0");
        assert_eq!(procargs_argv(&buf), ["claude", "agents"]);
        assert!(procargs_argv(b"\x01").is_empty());
    }

    #[test]
    fn reads_a_live_process_argv() {
        let mut child = Command::new("/bin/sleep").arg("5").spawn().unwrap();
        let argv = process_argv(child.id());
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(argv, ["/bin/sleep", "5"]);
    }

    #[test]
    fn last_words_skip_escape_sequences() {
        let out = b"\x1b[2J\x1b[1;31mError:\x1b[0m no session tui-poli\r\n\x1b[?25h";
        assert_eq!(last_words(out).as_deref(), Some("Error: no session tui-poli"));
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
