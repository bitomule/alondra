use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

use anyhow::{Context, Result};
use proc_macro2::{LineColumn, Span};
use quote::ToTokens;
use syn::spanned::Spanned;
use syn::{Attribute, Item};

pub const PASSES_ON_BASE: &str = "ablation: passes-on-base:";

struct Source<'a> {
    text: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> Source<'a> {
    fn new(text: &'a str) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { text, line_starts }
    }

    fn offset(&self, at: LineColumn) -> usize {
        let start = self.line_starts[at.line - 1];
        let line = &self.text[start..];
        start
            + line
                .char_indices()
                .nth(at.column)
                .map_or(line.len(), |(i, _)| i)
    }

    fn range(&self, span: Span) -> Range<usize> {
        self.offset(span.start())..self.offset(span.end())
    }
}

fn parse(text: &str) -> Result<syn::File> {
    syn::parse_file(text).context("not valid Rust")
}

pub fn attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

pub fn is_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("cfg") {
            return false;
        }
        let tokens = attr.meta.to_token_stream().to_string();
        let words: Vec<&str> = tokens
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .collect();
        words.contains(&"test") && !words.contains(&"not")
    })
}

pub fn is_test_fn(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .any(|attr| attr.path().segments.last().is_some_and(|s| s.ident == "test"))
}

fn is_test_item(item: &Item) -> bool {
    let attrs = attrs(item);
    is_cfg_test(attrs) || is_test_fn(attrs)
}

fn tokens(t: &impl ToTokens) -> String {
    t.to_token_stream().to_string()
}

fn item_name(item: &Item, unnamed: &mut HashMap<&'static str, usize>) -> String {
    let named = |kind: &str, ident: &syn::Ident| format!("{kind} {ident}");
    match item {
        Item::Fn(i) => named("fn", &i.sig.ident),
        Item::Mod(i) => named("mod", &i.ident),
        Item::Struct(i) => named("struct", &i.ident),
        Item::Enum(i) => named("enum", &i.ident),
        Item::Union(i) => named("union", &i.ident),
        Item::Trait(i) => named("trait", &i.ident),
        Item::TraitAlias(i) => named("trait", &i.ident),
        Item::Type(i) => named("type", &i.ident),
        Item::Const(i) => named("const", &i.ident),
        Item::Static(i) => named("static", &i.ident),
        Item::Use(i) => format!("use {}", tokens(&i.tree)),
        Item::Impl(i) => {
            let of = i
                .trait_
                .as_ref()
                .map(|(_, path, _)| format!("{} for ", tokens(path)))
                .unwrap_or_default();
            format!("impl {of}{}", tokens(&i.self_ty))
        }
        Item::Macro(i) => format!("macro {}", tokens(&i.mac)),
        _ => {
            let n = unnamed.entry("item").or_default();
            *n += 1;
            format!("item #{n}")
        }
    }
}

#[derive(Debug)]
struct TestItem {
    key: String,
    container: Vec<String>,
    range: Range<usize>,
}

fn collect_test_items(
    items: &[Item],
    container: &[String],
    src: &Source,
    out: &mut Vec<TestItem>,
) {
    let mut unnamed = HashMap::new();
    for item in items {
        if is_test_item(item) {
            let name = item_name(item, &mut unnamed);
            let mut key = container.join("::");
            key.push('/');
            key.push_str(&name);
            out.push(TestItem {
                key,
                container: container.to_vec(),
                range: src.range(item.span()),
            });
        } else if let Item::Mod(m) = item {
            if let Some((_, inner)) = &m.content {
                let mut path = container.to_vec();
                path.push(m.ident.to_string());
                collect_test_items(inner, &path, src, out);
            }
        }
    }
}

fn module_close(items: &[Item], path: &[String], src: &Source) -> Option<usize> {
    let (first, rest) = path.split_first()?;
    items.iter().find_map(|item| match item {
        Item::Mod(m) if m.ident == first => {
            let (brace, inner) = m.content.as_ref()?;
            if rest.is_empty() {
                Some(src.offset(brace.span.close().start()))
            } else {
                module_close(inner, rest, src)
            }
        }
        _ => None,
    })
}

/// The base file with every test item (a `#[cfg(test)]` item or a `#[test]` function) taken
/// from head: production code stays as the base had it, tests become what head has.
pub fn transplant(base: &str, head: &str) -> Result<String> {
    let base_file = parse(base).context("base")?;
    let head_file = parse(head).context("head")?;
    let base_src = Source::new(base);
    let head_src = Source::new(head);
    let mut base_items = Vec::new();
    collect_test_items(&base_file.items, &[], &base_src, &mut base_items);
    let mut head_items = Vec::new();
    collect_test_items(&head_file.items, &[], &head_src, &mut head_items);

    let head_by_key: HashMap<&str, &TestItem> =
        head_items.iter().map(|t| (t.key.as_str(), t)).collect();
    let base_keys: HashSet<&str> = base_items.iter().map(|t| t.key.as_str()).collect();

    let mut replacements: Vec<(Range<usize>, &str)> = base_items
        .iter()
        .map(|t| {
            let text = head_by_key
                .get(t.key.as_str())
                .map_or("", |h| &head[h.range.clone()]);
            (t.range.clone(), text)
        })
        .collect();

    let mut insertions: BTreeMap<usize, String> = BTreeMap::new();
    for t in head_items.iter().filter(|t| !base_keys.contains(t.key.as_str())) {
        let at = module_close(&base_file.items, &t.container, &base_src).unwrap_or(base.len());
        let block = insertions.entry(at).or_default();
        block.push('\n');
        block.push_str(&head[t.range.clone()]);
        block.push('\n');
    }

    replacements.sort_by_key(|(range, _)| range.start);
    let mut out = String::with_capacity(head.len().max(base.len()));
    let mut cursor = 0;
    let mut pending = insertions.into_iter().peekable();
    for (range, text) in replacements {
        while let Some((at, _)) = pending.peek() {
            if *at > range.start {
                break;
            }
            let (at, block) = pending.next().unwrap();
            out.push_str(&base[cursor..at]);
            out.push_str(&block);
            cursor = at;
        }
        out.push_str(&base[cursor..range.start]);
        out.push_str(text);
        cursor = range.end;
    }
    for (at, block) in pending {
        out.push_str(&base[cursor..at]);
        out.push_str(&block);
        cursor = at;
    }
    out.push_str(&base[cursor..]);
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestFn {
    pub path: String,
    pub tokens: String,
    pub passes_on_base: Option<String>,
}

fn passes_on_base(src: &Source, line_start: usize) -> Option<String> {
    src.text[..line_start]
        .lines()
        .rev()
        .map(str::trim)
        .take_while(|line| line.starts_with("//"))
        .find_map(|line| {
            let at = line.find(PASSES_ON_BASE)?;
            Some(line[at + PASSES_ON_BASE.len()..].trim().to_string())
        })
}

fn collect_test_fns(items: &[Item], container: &[String], src: &Source, out: &mut Vec<TestFn>) {
    for item in items {
        match item {
            Item::Fn(f) if is_test_fn(&f.attrs) => {
                let mut path = container.to_vec();
                path.push(f.sig.ident.to_string());
                let range = src.range(item.span());
                let line_start = src.text[..range.start].rfind('\n').map_or(0, |i| i + 1);
                out.push(TestFn {
                    path: path.join("::"),
                    tokens: tokens(f),
                    passes_on_base: passes_on_base(src, line_start),
                });
            }
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    let mut path = container.to_vec();
                    path.push(m.ident.to_string());
                    collect_test_fns(inner, &path, src, out);
                }
            }
            _ => {}
        }
    }
}

/// Every `#[test]` function in the file, with its module path inside the file.
pub fn test_fns(text: &str) -> Result<Vec<TestFn>> {
    let file = parse(text)?;
    let mut out = Vec::new();
    collect_test_fns(&file.items, &[], &Source::new(text), &mut out);
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    New,
    Changed,
}

/// The test functions head adds or whose code differs from the base (formatting and plain
/// comments do not count).
pub fn changed_tests(base: Option<&str>, head: &str) -> Result<Vec<(TestFn, Change)>> {
    let before: HashMap<String, String> = match base {
        Some(text) => test_fns(text)
            .context("base")?
            .into_iter()
            .map(|t| (t.path, t.tokens))
            .collect(),
        None => HashMap::new(),
    };
    Ok(test_fns(head)
        .context("head")?
        .into_iter()
        .filter_map(|t| match before.get(&t.path) {
            None => Some((t, Change::New)),
            Some(tokens) if *tokens != t.tokens => Some((t, Change::Changed)),
            Some(_) => None,
        })
        .collect())
}

/// `mod name;` declarations (no inline body) at the top level of a file, and inside its
/// inline modules, as (inline module path, name, `#[path]` value, under `cfg(test)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModDecl {
    pub inline: Vec<String>,
    pub name: String,
    pub path_attr: Option<String>,
    pub test: bool,
}

fn path_attr(attrs: &[Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if !attr.path().is_ident("path") {
            return None;
        }
        let syn::Meta::NameValue(nv) = &attr.meta else {
            return None;
        };
        match &nv.value {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) => Some(s.value()),
            _ => None,
        }
    })
}

fn collect_mod_decls(items: &[Item], inline: &[String], test: bool, out: &mut Vec<ModDecl>) {
    for item in items {
        let Item::Mod(m) = item else { continue };
        let test = test || is_cfg_test(&m.attrs);
        match &m.content {
            None => out.push(ModDecl {
                inline: inline.to_vec(),
                name: m.ident.to_string(),
                path_attr: path_attr(&m.attrs),
                test,
            }),
            Some((_, inner)) => {
                let mut path = inline.to_vec();
                path.push(m.ident.to_string());
                collect_mod_decls(inner, &path, test, out);
            }
        }
    }
}

pub fn mod_decls(text: &str) -> Result<Vec<ModDecl>> {
    let file = parse(text)?;
    let mut out = Vec::new();
    collect_mod_decls(&file.items, &[], false, &mut out);
    Ok(out)
}

struct Removable {
    range: Range<usize>,
    lines: Range<usize>,
    test_fn: Option<String>,
}

fn collect_removable(
    items: &[Item],
    container: &[String],
    in_test: bool,
    src: &Source,
    out: &mut Vec<Removable>,
) {
    for item in items {
        let test = in_test || is_test_item(item);
        if let Item::Mod(m) = item {
            if let Some((_, inner)) = &m.content {
                let mut path = container.to_vec();
                path.push(m.ident.to_string());
                collect_removable(inner, &path, test, src, out);
                continue;
            }
        }
        if !test {
            continue;
        }
        if let Item::Impl(imp) = item {
            for inner in &imp.items {
                let span = inner.span();
                out.push(Removable {
                    range: src.range(span),
                    lines: span.start().line..span.end().line + 1,
                    test_fn: None,
                });
            }
        }
        let test_fn = match item {
            Item::Fn(f) if is_test_fn(&f.attrs) => {
                let mut path = container.to_vec();
                path.push(f.sig.ident.to_string());
                Some(path.join("::"))
            }
            _ => None,
        };
        let span = item.span();
        out.push(Removable {
            range: src.range(span),
            lines: span.start().line..span.end().line + 1,
            test_fn,
        });
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Removal {
    pub text: String,
    /// Test functions taken out, as module paths inside the file.
    pub removed_tests: Vec<String>,
    /// Lines outside test code: removing code cannot fix them.
    pub unresolved: Vec<usize>,
}

/// Takes out the test code holding each of `lines` (a test, a test helper, one method of a
/// fake impl), so the remaining tests compile; `whole_file` says the file is all test code.
pub fn remove_at_lines(text: &str, lines: &[usize], whole_file: bool) -> Result<Removal> {
    let file = parse(text)?;
    let src = Source::new(text);
    let mut removable = Vec::new();
    collect_removable(&file.items, &[], whole_file, &src, &mut removable);

    let mut chosen: Vec<&Removable> = Vec::new();
    let mut unresolved = Vec::new();
    for &line in lines {
        let innermost = removable
            .iter()
            .filter(|r| r.lines.contains(&line))
            .min_by_key(|r| r.range.end - r.range.start);
        match innermost {
            Some(r) => chosen.push(r),
            None => unresolved.push(line),
        }
    }
    chosen.sort_by_key(|r| (r.range.start, std::cmp::Reverse(r.range.end)));
    chosen.dedup_by(|later, earlier| later.range.start < earlier.range.end);

    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut removed_tests = Vec::new();
    for r in chosen {
        out.push_str(&text[cursor..r.range.start]);
        cursor = r.range.end;
        removed_tests.extend(r.test_fn.clone());
    }
    out.push_str(&text[cursor..]);
    unresolved.sort_unstable();
    unresolved.dedup();
    Ok(Removal { text: out, removed_tests, unresolved })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_at_lines_takes_out_only_the_test_code_that_does_not_compile() {
        let text = "\
pub fn f() -> u8 { 1 }
#[cfg(test)]
mod tests {
    use super::new_api;
    struct Fake;
    impl Merger for Fake {
        fn old(&self) {}
        fn added(&self) {}
    }
    #[test]
    fn uses_new_api() {
        new_api();
    }
    #[test]
    fn plain() {
        assert_eq!(super::f(), 2);
    }
}
";
        let got = remove_at_lines(text, &[1, 4, 8, 12], false).unwrap();
        assert_eq!(got.removed_tests, vec!["tests::uses_new_api".to_string()]);
        assert_eq!(got.unresolved, vec![1]);
        assert!(!got.text.contains("new_api"));
        assert!(!got.text.contains("fn added"));
        assert!(got.text.contains("fn old"));
        assert!(got.text.contains("fn plain"));
        assert!(got.text.contains("pub fn f() -> u8 { 1 }"));
        syn::parse_file(&got.text).unwrap();

        let whole = remove_at_lines("fn helper() {}\n#[test]\nfn a() { helper() }\n", &[1], true).unwrap();
        assert!(whole.unresolved.is_empty());
        assert_eq!(whole.text, "\n#[test]\nfn a() { helper() }\n");
    }

    struct Case {
        name: &'static str,
        base: &'static str,
        head: &'static str,
        want: &'static str,
    }

    #[test]
    fn transplant_keeps_production_from_base_and_tests_from_head() {
        let cases = [
            Case {
                name: "inline test module replaced",
                base: "fn f() -> u8 { 1 }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn a() { assert_eq!(super::f(), 1); }\n}\n",
                head: "fn f() -> u8 { 2 }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn a() { assert_eq!(super::f(), 2); }\n}\n",
                want: "fn f() -> u8 { 1 }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn a() { assert_eq!(super::f(), 2); }\n}\n",
            },
            Case {
                name: "new test module appended",
                base: "fn f() -> u8 { 1 }\n",
                head: "fn f() -> u8 { 2 }\n#[cfg(test)]\nmod tests { #[test] fn a() {} }\n",
                want: "fn f() -> u8 { 1 }\n\n#[cfg(test)]\nmod tests { #[test] fn a() {} }\n",
            },
            Case {
                name: "production added right above the test module stays out",
                base: "fn f() {}\n#[cfg(test)]\nmod tests {}\n",
                head: "fn f() {}\nfn g() {}\n#[cfg(test)]\nmod tests { #[test] fn a() { super::g() } }\n",
                want: "fn f() {}\n#[cfg(test)]\nmod tests { #[test] fn a() { super::g() } }\n",
            },
            Case {
                name: "test module removed in head",
                base: "fn f() {}\n#[cfg(test)]\nmod tests {}\n",
                head: "fn f() {}\n",
                want: "fn f() {}\n\n",
            },
            Case {
                name: "nested cfg(test) inside an inline module",
                base: "mod a {\n    pub fn f() {}\n}\n",
                head: "mod a {\n    pub fn f() { g() }\n    #[cfg(test)]\n    mod tests {}\n}\n",
                want: "mod a {\n    pub fn f() {}\n\n#[cfg(test)]\n    mod tests {}\n}\n",
            },
            Case {
                name: "cfg(test) helper function and use kept by key",
                base: "#[cfg(test)]\nuse std::fmt;\nfn f() {}\n#[cfg(test)]\nfn helper() -> u8 { 1 }\n",
                head: "#[cfg(test)]\nuse std::fmt;\nfn f() { todo!() }\n#[cfg(test)]\nfn helper() -> u8 { 2 }\n",
                want: "#[cfg(test)]\nuse std::fmt;\nfn f() {}\n#[cfg(test)]\nfn helper() -> u8 { 2 }\n",
            },
            Case {
                name: "cfg(not(test)) is production",
                base: "#[cfg(not(test))]\nfn f() -> u8 { 1 }\n",
                head: "#[cfg(not(test))]\nfn f() -> u8 { 2 }\n",
                want: "#[cfg(not(test))]\nfn f() -> u8 { 1 }\n",
            },
            Case {
                name: "multibyte text before the test module",
                base: "const S: &str = \"ñandú\";\n#[cfg(test)]\nmod tests {}\n",
                head: "const S: &str = \"ñandú ü\";\n#[cfg(test)]\nmod tests { #[test] fn a() {} }\n",
                want: "const S: &str = \"ñandú\";\n#[cfg(test)]\nmod tests { #[test] fn a() {} }\n",
            },
        ];
        for case in cases {
            assert_eq!(
                transplant(case.base, case.head).unwrap(),
                case.want,
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn changed_tests_lists_new_and_edited_tests_only() {
        let base = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn same() { assert!(true); }\n    #[test]\n    fn edited() { assert_eq!(1, 1); }\n    #[test]\n    fn gone() {}\n}\n";
        let head = "#[cfg(test)]\nmod tests {\n    // a comment\n    #[test]\n    fn same() {\n        assert!(true);\n    }\n    #[test]\n    fn edited() { assert_eq!(1, 2); }\n    #[tokio::test]\n    async fn added() {}\n}\n";
        let got: Vec<(String, Change)> = changed_tests(Some(base), head)
            .unwrap()
            .into_iter()
            .map(|(t, c)| (t.path, c))
            .collect();
        assert_eq!(
            got,
            vec![
                ("tests::edited".to_string(), Change::Changed),
                ("tests::added".to_string(), Change::New),
            ]
        );
    }

    #[test]
    fn every_test_is_new_in_a_new_file() {
        let got = changed_tests(None, "#[test]\nfn a() {}\nmod m { #[test] fn b() {} }\n").unwrap();
        let paths: Vec<&str> = got.iter().map(|(t, _)| t.path.as_str()).collect();
        assert_eq!(paths, ["a", "m::b"]);
        assert!(got.iter().all(|(_, c)| *c == Change::New));
    }

    #[test]
    fn passes_on_base_marker_is_read_from_the_comment_above_the_test() {
        let head = "mod tests {\n    // ablation: passes-on-base: renamed only\n    #[test]\n    fn a() {}\n\n    // unrelated\n    #[test]\n    fn b() {}\n}\n";
        let fns = test_fns(head).unwrap();
        assert_eq!(fns[0].passes_on_base.as_deref(), Some("renamed only"));
        assert_eq!(fns[1].passes_on_base, None);
    }

    #[test]
    fn mod_decls_mark_test_modules_and_path_attributes() {
        let text = "mod a;\n#[cfg(test)]\nmod tests;\n#[cfg(test)]\n#[path = \"x/y_tests.rs\"]\nmod y;\n#[cfg(test)]\nmod inline {\n    mod deep;\n}\n";
        let got = mod_decls(text).unwrap();
        assert_eq!(
            got,
            vec![
                ModDecl { inline: vec![], name: "a".into(), path_attr: None, test: false },
                ModDecl { inline: vec![], name: "tests".into(), path_attr: None, test: true },
                ModDecl {
                    inline: vec![],
                    name: "y".into(),
                    path_attr: Some("x/y_tests.rs".into()),
                    test: true
                },
                ModDecl {
                    inline: vec!["inline".into()],
                    name: "deep".into(),
                    path_attr: None,
                    test: true
                },
            ]
        );
    }
}
