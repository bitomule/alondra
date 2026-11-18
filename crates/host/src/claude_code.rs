//! The Claude Code release Alondra runs through the Agent SDK, as `alondra doctor` and alondrad
//! read it: the pinned SDK needs 2.1.321 or later.

pub const MIN: [u32; 3] = [2, 1, 321];
pub const MIN_TEXT: &str = "2.1.321";

/// The release `claude --version` prints, as in `2.1.321 (Claude Code)`.
pub fn version(text: &str) -> Option<[u32; 3]> {
    let word = text.split_whitespace().next()?;
    let core = word.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u32>().ok());
    let version = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(version)
}

pub fn compatible(text: &str) -> bool {
    version(text).is_some_and(|version| version >= MIN)
}

/// The last lines of a command's output, enough to say why it failed.
pub fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    lines[lines.len().saturating_sub(3)..].join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alondra_needs_claude_code_2_1_321_or_later() {
        assert_eq!(version("2.1.321 (Claude Code)\n"), Some([2, 1, 321]));
        assert_eq!(version("2.1.290-dev.20261001.t101500.sha1a2b3c4 (Claude Code)"), Some([2, 1, 290]));
        assert!(compatible("2.1.321 (Claude Code)"));
        assert!(compatible("2.2.0 (Claude Code)"));
        assert!(!compatible("2.1.320 (Claude Code)"));
        assert!(!compatible("2.1.112 (Claude Code)"));
        assert!(!compatible("Claude Code"));
        assert_eq!(version("2.1"), None);
    }

    #[test]
    fn a_failure_is_told_by_its_last_lines() {
        let output = "tests/register.test.ts:\n(fail) reports\n\n 0 pass\n 1 fail\nRan 1 test across 1 file.";
        assert_eq!(tail(output), "0 pass / 1 fail / Ran 1 test across 1 file.");
        assert_eq!(tail(""), "");
    }
}
