//! `daemon.log`: the lines of `alondrad.log` in the window, read backwards from its end so a log of
//! gigabytes is never loaded, each kept only as its template with safe values.
use std::io::{Read, Seek, SeekFrom};

use super::redact::Redactor;
use super::templates::Catalog;
use super::time;

/// At most this much of the log is read, however long the window.
pub const READ_CAP: u64 = 64 * 1024 * 1024;
const CHUNK: u64 = 1024 * 1024;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct LogRead {
    /// Each line in the window, oldest first, with its time; an unstamped line has the time of the
    /// stamped line it follows.
    pub lines: Vec<Line>,
    pub bytes_read: u64,
    /// The cap was reached before the start of the window.
    pub capped: bool,
    /// The log was read back to a line older than the window.
    pub reached_start: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Line {
    pub at: i64,
    pub stamped: bool,
    pub message: String,
}

/// The lines of `log` from `from` (milliseconds) to its end.
pub fn read_back<R: Read + Seek>(log: &mut R, from: i64, cap: u64) -> std::io::Result<LogRead> {
    let length = log.seek(SeekFrom::End(0))?;
    let mut position = length;
    let mut pending: Vec<u8> = Vec::new();
    let mut backwards: Vec<(Option<i64>, String)> = Vec::new();
    let mut bytes_read = 0u64;
    let mut reached_start = false;
    'reading: while position > 0 {
        if bytes_read >= cap {
            break;
        }
        let size = CHUNK.min(position).min(cap - bytes_read);
        position -= size;
        log.seek(SeekFrom::Start(position))?;
        let mut chunk = vec![0u8; size as usize];
        log.read_exact(&mut chunk)?;
        bytes_read += size;
        chunk.extend_from_slice(&pending);
        pending = chunk;
        while let Some(newline) = pending.iter().rposition(|b| *b == b'\n') {
            let line = pending.split_off(newline + 1);
            pending.pop();
            if line.is_empty() {
                continue;
            }
            let (stamp, message) = split(&String::from_utf8_lossy(&line));
            if stamp.is_some_and(|at| at < from) {
                reached_start = true;
                break 'reading;
            }
            backwards.push((stamp, message));
        }
    }
    if !reached_start && position == 0 && !pending.is_empty() {
        let (stamp, message) = split(&String::from_utf8_lossy(&pending));
        if stamp.is_none_or(|at| at >= from) {
            backwards.push((stamp, message));
        }
    }
    let capped = !reached_start && position > 0;
    let mut lines = Vec::with_capacity(backwards.len());
    let mut last = None;
    for (stamp, message) in backwards.into_iter().rev() {
        match (stamp, last) {
            (Some(at), _) => {
                last = Some(at);
                lines.push(Line { at, stamped: true, message });
            }
            (None, Some(at)) => lines.push(Line { at, stamped: false, message }),
            (None, None) => {}
        }
    }
    Ok(LogRead { lines, bytes_read, capped, reached_start })
}

/// The lines of the window from `alondrad.log` and, when the window goes back further than it, from
/// the file the daemon rotated it into, older lines first.
pub fn read_window(home: &std::path::Path, from: i64, cap: u64) -> std::io::Result<LogRead> {
    let mut current = read_back(&mut std::fs::File::open(home.join(FILE))?, from, cap)?;
    if current.reached_start || current.capped {
        return Ok(current);
    }
    let Ok(mut rotated) = std::fs::File::open(home.join(ROTATED)) else { return Ok(current) };
    let mut older = read_back(&mut rotated, from, cap - current.bytes_read)?;
    older.lines.append(&mut current.lines);
    older.bytes_read += current.bytes_read;
    Ok(older)
}

pub const FILE: &str = "alondrad.log";
/// The same name as `alondra_core::daemon_log::ROTATED`, which the daemon rotates its log into.
pub const ROTATED: &str = "alondrad.log.1";

/// A line's time and its message: `<time> alondrad: <message>`, or the whole line when unstamped.
fn split(line: &str) -> (Option<i64>, String) {
    let line = line.trim_end_matches(['\n', '\r']);
    if let Some((stamp, rest)) = line.split_once(' ')
        && let Some(at) = time::parse_iso(stamp)
    {
        return (Some(at), rest.strip_prefix("alondrad: ").unwrap_or(rest).to_owned());
    }
    (None, line.to_owned())
}

#[derive(Debug, Default)]
pub struct Redacted {
    pub text: Vec<String>,
    pub without_template: u64,
}

/// Each line as `<UTC time> <message>` with its template's text and safe values only; an unstamped
/// line's time starts with `~`. A line no template matches is left out and counted.
pub fn redact(lines: &[Line], catalog: &Catalog, redactor: &mut Redactor) -> Redacted {
    let mut out = Redacted::default();
    for line in lines {
        match redactor.log_message(catalog, &line.message) {
            Some(message) => {
                let mark = if line.stamped { "" } else { "~" };
                out.text.push(format!("{mark}{} {message}", time::utc(line.at)));
            }
            None => out.without_template += 1,
        }
    }
    out
}
