use std::collections::BTreeMap;

use crate::items::mod_decls;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModFile {
    pub crate_dir: String,
    /// Module path inside its test binary, as libtest prints it.
    pub module: Vec<String>,
    /// Only compiled under `cfg(test)`, or an integration test.
    pub test: bool,
}

fn dirname(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn join(dir: &str, rest: &str) -> String {
    let mut parts: Vec<&str> = if dir.is_empty() { vec![] } else { dir.split('/').collect() };
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}

/// Every Rust file the crate compiles, reached from its roots (`src/lib.rs`, `src/main.rs`,
/// `tests/*.rs`) through `mod name;` declarations, honouring `#[path]`.
pub fn module_map(
    crate_dir: &str,
    files: &[String],
    read: &dyn Fn(&str) -> Option<String>,
) -> BTreeMap<String, ModFile> {
    let mut map = BTreeMap::new();
    let mut queue: Vec<(String, ModFile, bool)> = Vec::new();
    for root in ["src/lib.rs", "src/main.rs"] {
        let path = format!("{crate_dir}/{root}");
        if files.contains(&path) {
            let file = ModFile { crate_dir: crate_dir.into(), module: vec![], test: false };
            queue.push((path, file, true));
        }
    }
    let tests_dir = format!("{crate_dir}/tests/");
    for path in files {
        if let Some(rest) = path.strip_prefix(&tests_dir) {
            if rest.ends_with(".rs") && !rest.contains('/') {
                let file = ModFile { crate_dir: crate_dir.into(), module: vec![], test: true };
                queue.push((path.clone(), file, true));
            }
        }
    }

    while let Some((path, file, owns_dir)) = queue.pop() {
        if map.contains_key(&path) {
            continue;
        }
        let Some(text) = read(&path) else { continue };
        map.insert(path.clone(), file.clone());
        let Ok(decls) = mod_decls(&text) else { continue };
        let here = dirname(&path);
        let children_dir = if owns_dir {
            here.to_string()
        } else {
            let stem = path.rsplit('/').next().unwrap_or("").trim_end_matches(".rs");
            join(here, stem)
        };
        for decl in decls {
            let inline_dir = join(&children_dir, &decl.inline.join("/"));
            let (child, owns) = match &decl.path_attr {
                Some(p) if decl.inline.is_empty() => (join(here, p), true),
                Some(p) => (join(&inline_dir, p), true),
                None => {
                    let flat = join(&inline_dir, &format!("{}.rs", decl.name));
                    if files.contains(&flat) {
                        (flat, false)
                    } else {
                        (join(&inline_dir, &format!("{}/mod.rs", decl.name)), true)
                    }
                }
            };
            let mut module = file.module.clone();
            module.extend(decl.inline.iter().cloned());
            module.push(decl.name.clone());
            let child_file = ModFile {
                crate_dir: crate_dir.into(),
                module,
                test: file.test || decl.test,
            };
            queue.push((child, child_file, owns));
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_test_modules_through_mod_path_and_inline_declarations() {
        let tree: BTreeMap<&str, &str> = BTreeMap::from([
            ("c/src/lib.rs", "pub mod domain;\nmod views;\n#[cfg(test)]\n#[path = \"extra/odd.rs\"]\nmod odd;\n"),
            ("c/src/domain.rs", "#[cfg(test)]\nmod card_tests;\nmod store;\n"),
            ("c/src/domain/card_tests.rs", "mod helpers;\n"),
            ("c/src/domain/card_tests/helpers.rs", ""),
            ("c/src/domain/store.rs", ""),
            ("c/src/views/mod.rs", "#[cfg(test)]\nmod tests;\n"),
            ("c/src/views/tests.rs", ""),
            ("c/src/extra/odd.rs", ""),
            ("c/tests/flow.rs", "mod support;\n"),
            ("c/tests/support/mod.rs", ""),
            ("c/src/orphan.rs", ""),
        ]);
        let files: Vec<String> = tree.keys().map(|k| k.to_string()).collect();
        let read = |p: &str| tree.get(p).map(|s| s.to_string());
        let map = module_map("c", &files, &read);
        let summary: Vec<(&str, String, bool)> = map
            .iter()
            .map(|(p, f)| (p.as_str(), f.module.join("::"), f.test))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("c/src/domain.rs", "domain".into(), false),
                ("c/src/domain/card_tests.rs", "domain::card_tests".into(), true),
                ("c/src/domain/card_tests/helpers.rs", "domain::card_tests::helpers".into(), true),
                ("c/src/domain/store.rs", "domain::store".into(), false),
                ("c/src/extra/odd.rs", "odd".into(), true),
                ("c/src/lib.rs", "".into(), false),
                ("c/src/views/mod.rs", "views".into(), false),
                ("c/src/views/tests.rs", "views::tests".into(), true),
                ("c/tests/flow.rs", "".into(), true),
                ("c/tests/support/mod.rs", "support".into(), true),
            ]
        );
    }
}
