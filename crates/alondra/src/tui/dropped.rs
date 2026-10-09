//! Files dropped onto the terminal. A terminal delivers a drop as pasted text: Terminal.app
//! escapes spaces with `\` or wraps the path in single quotes, others use double quotes, and
//! kitty sends `file://` URIs. Several files arrive separated by spaces or new lines.

use std::path::PathBuf;

/// The files a paste names, when the whole paste is nothing but paths to existing files.
/// Anything else (a sentence, a path that does not exist, a directory) is text to type.
pub fn dropped_files(pasted: &str) -> Option<Vec<PathBuf>> {
    let pasted = pasted.trim();
    let as_words = || -> Option<Vec<PathBuf>> {
        let words = shell_words(pasted)?;
        if words.is_empty() {
            return None;
        }
        words
            .iter()
            .map(|w| file_path(w).filter(|p| p.is_file()))
            .collect()
    };
    as_words().or_else(|| file_path(pasted).filter(|p| p.is_file()).map(|p| vec![p]))
}

fn file_path(word: &str) -> Option<PathBuf> {
    let path = match word.strip_prefix("file://") {
        Some(rest) => percent_decode(rest.strip_prefix("localhost").unwrap_or(rest))?,
        None => word.to_string(),
    };
    let path = match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(PathBuf::from)?.join(rest),
        None => PathBuf::from(path),
    };
    path.is_absolute().then_some(path)
}

/// Splits like a shell: quotes group, a backslash escapes the next character. `None` when a
/// quote is left open, which is not a drop.
fn shell_words(s: &str) -> Option<Vec<String>> {
    let mut words = vec![];
    let mut word: Option<String> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => w.push(c),
                    }
                }
            }
            '"' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => w.push(chars.next()?),
                        c => w.push(c),
                    }
                }
            }
            '\\' => word.get_or_insert_with(String::new).push(chars.next()?),
            c if c.is_whitespace() => words.extend(word.take()),
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    Some(words)
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

pub fn is_image(name: &str) -> bool {
    let ext = std::path::Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    [
        "png", "jpg", "jpeg", "gif", "webp", "heic", "heif", "tif", "tiff", "bmp",
    ]
    .contains(&ext.as_str())
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Upload {
    Sending,
    Stored(alondra_core::domain::AttachmentRef),
}

/// The references of the chips already stored; one still copying is never sent half-way.
pub fn stored(chips: &[Chip]) -> Vec<alondra_core::domain::AttachmentRef> {
    chips
        .iter()
        .filter_map(|c| match &c.upload {
            Upload::Stored(a) => Some(a.clone()),
            Upload::Sending => None,
        })
        .collect()
}

/// A file attached in the composer: its upload goes on while the user keeps typing.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Chip {
    pub seq: u64,
    pub name: String,
    pub bytes: u64,
    pub upload: Upload,
}

impl Chip {
    pub fn image(&self) -> bool {
        is_image(&self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(crate::test_support::Directory);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(_tag: &str, files: &[&str]) -> Scratch {
        let dir = crate::test_support::Directory::new();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in files {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        Scratch(dir)
    }

    #[test]
    fn every_way_a_terminal_delivers_a_dropped_file_is_recognised() {
        let s = scratch(
            "forms",
            &[
                "Captura de pantalla 2026-09-24.png",
                "it's.txt",
                "plain.pdf",
            ],
        );
        let d = s.0.to_string_lossy().to_string();
        let shot = s.0.join("Captura de pantalla 2026-09-24.png");
        let cases = [
            (
                "backslash-escaped spaces (Terminal.app, iTerm2, Ghostty)",
                format!("{d}/Captura\\ de\\ pantalla\\ 2026-09-24.png"),
                vec![shot.clone()],
            ),
            (
                "single quotes (Terminal.app)",
                format!("'{d}/Captura de pantalla 2026-09-24.png'"),
                vec![shot.clone()],
            ),
            (
                "double quotes",
                format!("\"{d}/Captura de pantalla 2026-09-24.png\""),
                vec![shot.clone()],
            ),
            (
                "file:// URI (kitty)",
                format!(
                    "file://{}/Captura%20de%20pantalla%202026-09-24.png",
                    d.replace(' ', "%20")
                ),
                vec![shot.clone()],
            ),
            (
                "file://localhost URI",
                format!("file://localhost{d}/plain.pdf"),
                vec![s.0.join("plain.pdf")],
            ),
            (
                "trailing space some terminals add",
                format!("{d}/plain.pdf "),
                vec![s.0.join("plain.pdf")],
            ),
            (
                "an apostrophe escaped",
                format!("{d}/it\\'s.txt"),
                vec![s.0.join("it's.txt")],
            ),
            (
                "two files at once",
                format!("{d}/plain.pdf {d}/Captura\\ de\\ pantalla\\ 2026-09-24.png"),
                vec![s.0.join("plain.pdf"), shot.clone()],
            ),
            (
                "unquoted, as a terminal that does not escape sends it",
                format!("{d}/Captura de pantalla 2026-09-24.png"),
                vec![shot.clone()],
            ),
            (
                "one per line",
                format!("{d}/plain.pdf\n'{d}/it'\\''s.txt'"),
                vec![s.0.join("plain.pdf"), s.0.join("it's.txt")],
            ),
        ];
        for (case, pasted, want) in cases {
            assert_eq!(dropped_files(&pasted), Some(want), "{case}: {pasted}");
        }
    }

    #[test]
    fn ordinary_text_stays_text() {
        let s = scratch("text", &["a.png"]);
        let d = s.0.to_string_lossy().to_string();
        let cases = [
            ("a sentence", "mira el fallo en la línea 3".to_string()),
            (
                "a path that does not exist",
                "/no/such/file.png".to_string(),
            ),
            ("a directory", d.clone()),
            ("a path inside a sentence", format!("mira {d}/a.png")),
            ("a relative path", "a.png".to_string()),
            ("an open quote", format!("'{d}/a.png")),
            ("nothing", "   ".to_string()),
        ];
        for (case, pasted) in cases {
            assert_eq!(dropped_files(&pasted), None, "{case}: {pasted}");
        }
    }
}
