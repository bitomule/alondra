//! The daemon's log in HOME: `alondrad.log`, and `alondrad.log.1` with the lines before the
//! last rotation. The daemon rotates it once it reaches [`ROTATE_AT`], so both stay bounded.
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::Path;

pub const FILE: &str = "alondrad.log";
pub const ROTATED: &str = "alondrad.log.1";
pub const ROTATE_AT: u64 = 10 * 1024 * 1024;

/// The last `lines` lines of the log, continuing into `alondrad.log.1` when the current file
/// holds fewer, reading at most `max_bytes` from the two.
pub fn tail(home: &Path, lines: usize, max_bytes: u64) -> std::io::Result<Vec<String>> {
    let (mut kept, read, whole) = end_of(&home.join(FILE), max_bytes)?;
    if kept.len() < lines && whole && read < max_bytes {
        let (mut older, _, _) = end_of(&home.join(ROTATED), max_bytes - read)?;
        older.append(&mut kept);
        kept = older;
    }
    Ok(kept.split_off(kept.len().saturating_sub(lines)))
}

/// The whole lines in the last `max_bytes` of `path`, the bytes read, and whether that was the
/// whole file.
fn end_of(path: &Path, max_bytes: u64) -> std::io::Result<(Vec<String>, u64, bool)> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0, true)),
        Err(error) => return Err(error),
    };
    let len = file.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start.saturating_sub(1)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let mut text = bytes.as_slice();
    if start > 0 {
        text.skip_until(b'\n')?;
    }
    let kept = String::from_utf8_lossy(text).lines().map(str::to_string).collect();
    Ok((kept, len - start, start == 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(range: std::ops::RangeInclusive<u32>) -> String {
        range.map(|n| format!("line {n}\n")).collect()
    }

    #[test]
    fn the_tail_after_a_rotation_continues_into_the_rotated_file() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(ROTATED), numbered(1..=5)).unwrap();
        std::fs::write(home.join(FILE), numbered(6..=7)).unwrap();
        assert_eq!(tail(&home, 4, 1024).unwrap(), ["line 4", "line 5", "line 6", "line 7"]);
        assert_eq!(tail(&home, 2, 1024).unwrap(), ["line 6", "line 7"]);
        assert_eq!(tail(&home, 100, 1024).unwrap().len(), 7);
    }

    #[test]
    fn the_tail_reads_no_more_than_its_budget_and_drops_the_cut_line() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(ROTATED), numbered(1..=5)).unwrap();
        std::fs::write(home.join(FILE), numbered(6..=9)).unwrap();
        assert_eq!(tail(&home, 100, 10).unwrap(), ["line 9"], "a full current file never reaches the rotated one");
        assert_eq!(tail(&home, 100, 35).unwrap(), ["line 5", "line 6", "line 7", "line 8", "line 9"]);
    }

    #[test]
    fn a_missing_log_has_no_lines() {
        let home = crate::test_support::Directory::new();
        assert!(tail(&home, 10, 1024).unwrap().is_empty());
    }
}
