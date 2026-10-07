use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, Result};

use crate::items::{Change, changed_tests, transplant};
use crate::modules::{ModFile, module_map};

pub struct Tree<'a> {
    pub files: &'a [String],
    pub read: &'a dyn Fn(&str) -> Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub crate_dir: String,
    /// Full libtest name, e.g. `domain::tests::merges_at_once`.
    pub name: String,
    pub file: String,
    pub change: Change,
    pub passes_on_base: Option<String>,
    /// False when the test lives in a production file the base does not have: without the
    /// change the test does not exist at all.
    pub exists_without_change: bool,
}

#[derive(Debug, Default)]
pub struct Plan {
    /// What the ablated tree holds where it differs from head: `None` deletes the file.
    pub writes: BTreeMap<String, Option<String>>,
    pub targets: Vec<Target>,
    /// Changed tests in other languages: listed, not ablated.
    pub uncovered: Vec<String>,
    /// Files of the changed crates that only compile into tests.
    pub test_files: BTreeSet<String>,
}

fn crate_dir(path: &str) -> Option<String> {
    let rest = path.strip_prefix("crates/")?;
    let (name, _) = rest.split_once('/')?;
    Some(format!("crates/{name}"))
}

fn is_test_data(path: &str) -> bool {
    path.starts_with("fixtures/")
        || path.contains("/fixtures/")
        || path.contains("/tests/")
        || path.contains("/testdata/")
}

fn is_uncovered_test(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    (path.ends_with(".swift") && (path.contains("Tests/") || file.ends_with("Tests.swift")))
        || (file.ends_with(".py") && (file.starts_with("test_") || file.ends_with("_test.py")))
        || file.ends_with("_test.sh")
        || file.contains(".spec.")
        || file.contains(".test.")
}

fn is_build_file(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    file == "BUILD.bazel" || file == "BUILD"
}

struct Maps<'a> {
    base: Tree<'a>,
    head: Tree<'a>,
    cache: HashMap<String, (BTreeMap<String, ModFile>, BTreeMap<String, ModFile>)>,
}

impl Maps<'_> {
    fn of(&mut self, krate: &str) -> &(BTreeMap<String, ModFile>, BTreeMap<String, ModFile>) {
        self.cache.entry(krate.to_string()).or_insert_with(|| {
            (
                module_map(krate, self.base.files, self.base.read),
                module_map(krate, self.head.files, self.head.read),
            )
        })
    }
}

/// The tree "without the change": production code from the base, tests, test data and
/// build files from head; and the tests head adds or edits, which must fail on that tree.
pub fn plan(changed: &[String], base: Tree, head: Tree) -> Result<Plan> {
    let read_base = base.read;
    let read_head = head.read;
    let mut maps = Maps { base, head, cache: HashMap::new() };
    let mut plan = Plan::default();

    for path in changed {
        let base_text = read_base(path);
        let head_text = read_head(path);
        let new_data_file = base_text.is_none() && !path.ends_with(".rs");
        let take_base = |plan: &mut Plan| {
            if base_text != head_text && !new_data_file {
                plan.writes.insert(path.clone(), base_text.clone());
            }
        };

        if is_uncovered_test(path) {
            plan.uncovered.push(path.clone());
        }
        if is_build_file(path) {
            continue;
        }
        let rust_crate = crate_dir(path).filter(|_| path.ends_with(".rs"));
        let Some(krate) = rust_crate else {
            if !is_test_data(path) {
                take_base(&mut plan);
            }
            continue;
        };

        let (base_map, head_map) = maps.of(&krate);
        let crate_in_base = !base_map.is_empty();
        let base_info = base_map.get(path).cloned();
        let head_info = head_map.get(path).cloned();
        let is_test = head_info.as_ref().is_some_and(|f| f.test)
            || base_info.as_ref().is_some_and(|f| f.test);

        if !is_test {
            match (&base_text, &head_text) {
                (Some(b), Some(h)) => {
                    let ablated = transplant(b, h).with_context(|| path.clone())?;
                    if ablated != *h {
                        plan.writes.insert(path.clone(), Some(ablated));
                    }
                }
                _ => take_base(&mut plan),
            }
        }

        let (Some(info), Some(head_text)) = (head_info, &head_text) else { continue };
        let base_for_diff = base_text.as_deref().filter(|_| base_info.is_some());
        let exists_without_change = crate_in_base && (is_test || base_text.is_some());
        for (test, change) in changed_tests(base_for_diff, head_text).with_context(|| path.clone())? {
            let mut name = info.module.clone();
            name.push(test.path);
            plan.targets.push(Target {
                crate_dir: krate.clone(),
                name: name.join("::"),
                file: path.clone(),
                change,
                passes_on_base: test.passes_on_base,
                exists_without_change,
            });
        }
    }
    for (_, head_map) in maps.cache.values() {
        plan.test_files
            .extend(head_map.iter().filter(|(_, f)| f.test).map(|(p, _)| p.clone()));
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries.iter().map(|(p, t)| (p.to_string(), t.to_string())).collect()
    }

    #[test]
    fn tests_of_a_crate_the_base_does_not_have_cannot_run_without_the_change() {
        let base = tree(&[("crates/old/src/lib.rs", "")]);
        let head = tree(&[
            ("crates/old/src/lib.rs", ""),
            ("crates/new/src/lib.rs", "pub fn f() {}\n"),
            ("crates/new/tests/flow.rs", "#[test]\nfn flow() {}\n"),
        ]);
        let changed: Vec<String> =
            ["crates/new/src/lib.rs", "crates/new/tests/flow.rs"].map(String::from).to_vec();
        let base_files: Vec<String> = base.keys().cloned().collect();
        let head_files: Vec<String> = head.keys().cloned().collect();
        let read_base = |p: &str| base.get(p).cloned();
        let read_head = |p: &str| head.get(p).cloned();
        let plan = plan(
            &changed,
            Tree { files: &base_files, read: &read_base },
            Tree { files: &head_files, read: &read_head },
        )
        .unwrap();
        assert_eq!(plan.targets.len(), 1);
        assert!(!plan.targets[0].exists_without_change);
    }

    #[test]
    fn plan_reverts_production_keeps_tests_and_lists_targets() {
        let base = tree(&[
            ("crates/c/src/lib.rs", "pub mod merge;\n#[cfg(test)]\nmod merge_tests;\n"),
            ("crates/c/src/merge.rs", "pub fn wait() -> u32 { 300 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn old() {}\n}\n"),
            ("crates/c/src/merge_tests.rs", "#[test]\nfn kept() {}\n"),
            ("crates/c/BUILD.bazel", "base"),
            ("plugin/skill.md", "base doc"),
            ("fixtures/x.json", "{}"),
            ("crates/c/schema.sql", "base schema"),
        ]);
        let head = tree(&[
            ("crates/c/src/lib.rs", "pub mod merge;\nmod fresh;\n#[cfg(test)]\nmod merge_tests;\n"),
            ("crates/c/src/merge.rs", "pub fn wait() -> u32 { 0 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn old() {}\n    #[test]\n    fn no_wait() { assert_eq!(super::wait(), 0); }\n}\n"),
            ("crates/c/src/merge_tests.rs", "#[test]\nfn kept() {}\n#[test]\nfn also_new() {}\n"),
            ("crates/c/src/fresh.rs", "#[cfg(test)]\nmod tests { #[test] fn brand() {} }\n"),
            ("crates/c/tests/flow.rs", "#[test]\nfn end_to_end() {}\n"),
            ("crates/c/BUILD.bazel", "head"),
            ("plugin/skill.md", "head doc"),
            ("fixtures/x.json", "{\"a\":1}"),
            ("apple/Feed/Tests/FeedTests.swift", "func testA() {}"),
            ("crates/c/schema.sql", "head schema"),
            ("crates/c/restore.sh", "new data the head BUILD needs"),
        ]);
        let changed: Vec<String> = [
            "apple/Feed/Tests/FeedTests.swift",
            "crates/c/BUILD.bazel",
            "crates/c/restore.sh",
            "crates/c/schema.sql",
            "crates/c/src/fresh.rs",
            "crates/c/src/lib.rs",
            "crates/c/src/merge.rs",
            "crates/c/src/merge_tests.rs",
            "crates/c/tests/flow.rs",
            "fixtures/x.json",
            "plugin/skill.md",
        ]
        .map(String::from)
        .to_vec();
        let base_files: Vec<String> = base.keys().cloned().collect();
        let head_files: Vec<String> = head.keys().cloned().collect();
        let read_base = |p: &str| base.get(p).cloned();
        let read_head = |p: &str| head.get(p).cloned();
        let plan = plan(
            &changed,
            Tree { files: &base_files, read: &read_base },
            Tree { files: &head_files, read: &read_head },
        )
        .unwrap();

        assert_eq!(
            plan.writes,
            BTreeMap::from([
                ("crates/c/schema.sql".to_string(), Some("base schema".to_string())),
                ("crates/c/src/fresh.rs".to_string(), None),
                ("crates/c/src/lib.rs".to_string(), base.get("crates/c/src/lib.rs").cloned()),
                (
                    "crates/c/src/merge.rs".to_string(),
                    Some("pub fn wait() -> u32 { 300 }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn old() {}\n    #[test]\n    fn no_wait() { assert_eq!(super::wait(), 0); }\n}\n".to_string())
                ),
                ("plugin/skill.md".to_string(), Some("base doc".to_string())),
            ])
        );
        let targets: Vec<(&str, &str, bool)> = plan
            .targets
            .iter()
            .map(|t| (t.crate_dir.as_str(), t.name.as_str(), t.exists_without_change))
            .collect();
        assert_eq!(
            targets,
            vec![
                ("crates/c", "fresh::tests::brand", false),
                ("crates/c", "merge::tests::no_wait", true),
                ("crates/c", "merge_tests::also_new", true),
                ("crates/c", "end_to_end", true),
            ]
        );
        assert_eq!(plan.uncovered, vec!["apple/Feed/Tests/FeedTests.swift".to_string()]);
    }
}
