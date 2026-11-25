use std::io::Write;

/// Mouse, focus, paste and keyboard-protocol modes a full-screen program may leave switched on,
/// then the main screen and the cursor back.
const RESET_MODES: &[u8] =
    b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?1004l\x1b[?2004l\x1b[<99u\x1b[=0;1u\x1b[>4;0m\x1b[?1049l\x1b[?25h";

pub fn reset_terminal_modes() {
    let mut out = std::io::stdout();
    let _ = out.write_all(RESET_MODES);
    let _ = out.flush();
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
