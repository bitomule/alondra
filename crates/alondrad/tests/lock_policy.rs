//! Every lock of the daemon recovers or exits when its holder panicked (`src/locks.rs`): an
//! `unwrap`, `expect` or `map_err` on a lock in production code makes every later reader panic or
//! fail, as the machine watch's lock did on 25 October.
use std::path::{Path, PathBuf};

const SOURCES: [&str; 3] = ["crates/alondrad/src", "crates/daemon-support/src", "crates/daemon-engine/src"];
/// What marks an item that only tests compile: alondrad's tests also build the support and engine
/// crates with their `test-hooks` feature.
const TEST_ONLY: [&str; 2] = ["#[cfg(test)]", "#[cfg(any(test, feature = \"test-hooks\"))]"];

fn next_test_only(text: &str) -> Option<(usize, usize)> {
    TEST_ONLY.iter().filter_map(|marker| text.find(marker).map(|at| (at, marker.len()))).min()
}

fn rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

/// The files of modules declared `#[cfg(test)] mod name;`.
fn test_modules(files: &[PathBuf]) -> Vec<PathBuf> {
    let mut modules = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(file).unwrap();
        let dir = file.parent().unwrap();
        let stem = file.file_stem().unwrap().to_str().unwrap();
        let base = if ["lib", "main", "mod"].contains(&stem) { dir.to_path_buf() } else { dir.join(stem) };
        let mut rest = text.as_str();
        while let Some((at, len)) = next_test_only(rest) {
            rest = &rest[at + len..];
            let mut item = rest.trim_start();
            let mut path = None;
            if let Some(attribute) = item.strip_prefix("#[path = \"") {
                let end = attribute.find('"').unwrap();
                path = Some(dir.join(&attribute[..end]));
                item = attribute[end..].split_once(']').unwrap().1.trim_start();
            }
            let item = item.trim_start_matches("pub(crate) ").trim_start_matches("pub(super) ").trim_start_matches("pub ");
            let Some(declared) = item.strip_prefix("mod ") else { continue };
            let name: String = declared.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if !declared[name.len()..].trim_start().starts_with(';') {
                continue;
            }
            match path {
                Some(path) => modules.push(path),
                None => {
                    modules.push(base.join(format!("{name}.rs")));
                    modules.push(base.join(&name).join("mod.rs"));
                }
            }
        }
    }
    modules
}

/// `text` with every `#[cfg(test)]` item blanked, keeping its lines so lines still count right.
fn without_test_items(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut kept = String::with_capacity(text.len());
    let mut from = 0;
    while let Some((found, len)) = next_test_only(&text[from..]) {
        let start = from + found;
        kept.push_str(&text[from..start]);
        let (mut at, mut braces, mut nesting, mut opened) = (start + len, 0i32, 0i32, false);
        while at < bytes.len() {
            match bytes[at] {
                b'{' => { braces += 1; opened = true; }
                b'}' => {
                    braces -= 1;
                    if opened && braces == 0 { at += 1; break; }
                }
                b'(' | b'[' => nesting += 1,
                b')' | b']' => nesting -= 1,
                b'<' => nesting += 1,
                b'>' if !matches!(bytes[at - 1], b'-' | b'=') => nesting -= 1,
                b';' if !opened => { at += 1; break; }
                b',' if !opened && nesting == 0 => { at += 1; break; }
                _ => {}
            }
            at += 1;
        }
        kept.extend(std::iter::repeat_n('\n', text[start..at].matches('\n').count()));
        from = at;
    }
    kept.push_str(&text[from..]);
    kept
}

/// Where `text` takes a lock and unwraps it, expects it or turns its poison into an error.
fn poison_panics(text: &str) -> Vec<usize> {
    let mut lines = Vec::new();
    for call in [".lock()", ".read()", ".write()", ".wait(", ".wait_timeout(", ".wait_while("] {
        let mut from = 0;
        while let Some(found) = text[from..].find(call) {
            let start = from + found;
            let mut at = start + call.len();
            if call.ends_with('(') && text[at..].starts_with(')') {
                from = at;
                continue;
            }
            if call.ends_with('(') {
                let mut depth = 1;
                while at < text.len() && depth > 0 {
                    match text.as_bytes()[at] { b'(' => depth += 1, b')' => depth -= 1, _ => {} }
                    at += 1;
                }
            }
            let after = text[at..].trim_start();
            if [".unwrap()", ".expect(", ".map_err("].iter().any(|then| after.starts_with(then)) {
                lines.push(text[..start].matches('\n').count() + 1);
            }
            from = start + call.len();
        }
    }
    lines.sort();
    lines
}

fn is_test_file(path: &Path) -> bool {
    let name = path.file_name().unwrap().to_str().unwrap();
    name == "tests.rs" || name.ends_with("_tests.rs") || path.components().any(|part| part.as_os_str() == "tests")
}

#[test]
fn no_lock_of_the_daemon_panics_every_later_reader() {
    let mut files = Vec::new();
    for sources in SOURCES {
        let before = files.len();
        rust_files(Path::new(sources), &mut files);
        assert!(files.len() > before, "{sources} is not in this test's runfiles");
    }
    assert!(files.len() > 100, "the daemon's sources are not in this test's runfiles: {}", files.len());
    let tests = test_modules(&files);
    let mut found = Vec::new();
    for file in files.iter().filter(|file| !is_test_file(file) && !tests.contains(file)) {
        let text = without_test_items(&std::fs::read_to_string(file).unwrap());
        found.extend(poison_panics(&text).into_iter().map(|line| format!("{}:{line}", file.display())));
    }
    assert!(found.is_empty(), "take these locks with `crate::locks` (lock_or_recover or lock_or_exit, \
        and say why at the lock):\n{}", found.join("\n"));
}

#[test]
fn the_panic_check_sees_a_lock_unwrapped_over_lines_and_ignores_test_items() {
    let text = "fn a() {\n    x.lock()\n        .unwrap();\n}\n#[cfg(test)]\nfn b() { y.lock().unwrap(); }\n\
        struct S {\n    #[cfg(test)]\n    hook: Mutex<Option<Box<dyn Fn(u8, u8)>>>,\n}\nfn c() { z.read().expect(\"poisoned\"); \
        w.wait_timeout(g, d).map_err(|_| e)?; child.wait().map_err(|_| e)?; }\n\
        #[cfg(any(test, feature = \"test-hooks\"))]\nfn d() { v.lock().unwrap(); }\n";
    assert_eq!(poison_panics(&without_test_items(text)), [2, 11, 11]);
}
