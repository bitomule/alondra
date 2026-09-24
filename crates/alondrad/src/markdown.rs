/// The fence still open after `line`: `None` outside code, or the marker (```` ``` ````, `~~~`…) that closes it.
pub fn fence_after(open: Option<String>, line: &str) -> Option<String> {
    let line = line.trim();
    let Some(mark) = line.chars().next().filter(|c| matches!(c, '`' | '~')) else { return open };
    let run = line.chars().take_while(|&c| c == mark).count();
    if run < 3 {
        return open;
    }
    match open {
        None => Some(mark.to_string().repeat(run)),
        Some(m) if m.starts_with(mark) && run >= m.len() && line[run..].trim().is_empty() => None,
        other => other,
    }
}

/// Trimmed lines that read as prose: no code, no fence markers, no table rows.
pub fn prose_lines(message: &str) -> Vec<&str> {
    let mut fence = None;
    message
        .lines()
        .map(str::trim)
        .filter(|line| {
            let was_open = fence.is_some();
            fence = fence_after(fence.take(), line);
            !was_open && fence.is_none() && !is_table_row(line)
        })
        .collect()
}

fn is_table_row(line: &str) -> bool {
    line.strip_prefix('|').is_some_and(|rest| rest.contains('|'))
}
