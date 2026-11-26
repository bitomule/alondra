#!/usr/bin/env python3
"""`tools/public_surface.py` lists the same surface however a crate lays its code out."""

import importlib.util
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("public_surface", Path(__file__).resolve().parents[1] / "public_surface.py")
public_surface = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(public_surface)

ONE_FILE = """
//! pub fn in_a_comment() {}
pub trait Files {
    fn paths(&self) -> Vec<String>;
    fn read(&self, path: &str) -> Option<Vec<u8>>;
}

#[derive(Debug, Clone)]
pub struct Problem {
    pub code: Code,
    pub line: usize,
    hidden: bool,
}

pub enum Code { TooLarge, Other(String), Named { line: usize } }

pub const LIMIT: usize = 262144;

pub fn load(files: &dyn Files) -> Result<Project, Vec<Problem>> {
    let note = "pub fn in_a_string() {}";
    todo!()
}

pub(crate) fn internal() {}

fn private() {}

pub struct Project {}

impl Project {
    pub fn report(&self, results: Option<&TestResults>) -> Report { todo!() }
    fn helper(&self) {}
    pub(crate) fn inner(&self) {}
}

impl Files for Project {
    fn paths(&self) -> Vec<String> { Vec::new() }
    fn read(&self, path: &str) -> Option<Vec<u8>> { None }
}
"""

EXPECTED = """const LIMIT: usize
enum Code { TooLarge, Other, Named }
fn load(&dyn Files) -> Result<Project, Vec<Problem>>
impl Files for Project
impl Project: fn report(&self, Option<&TestResults>) -> Report
struct Problem { code: Code, line: usize }
struct Project { }
trait Files { fn paths(&self) -> Vec<String>; fn read(&self, &str) -> Option<Vec<u8>> }
"""

SPLIT = {
    "lib.rs": """
mod model;
mod project;
pub use model::{Code, Problem};
pub use project::Project;
pub trait Files { fn paths( &self ) -> Vec< String >; fn read(&self, name: &str) -> Option<Vec<u8>>; }
pub const LIMIT : usize = 1 << 18;
pub fn load(
    source: &dyn Files,
) -> Result<Project, Vec<Problem>>
{
    project::load(source)
}
""",
    "model.rs": """
pub struct Problem { pub code: Code, pub line: usize, hidden: bool }
pub enum Code {
    TooLarge,
    Other(String),
    Named { line: usize },
}
""",
    "project.rs": """
pub struct Project {}
impl Project {
    pub fn report(&self, given: Option<&TestResults>) -> Report { todo!() }
}
impl Files for Project { fn paths(&self) -> Vec<String> { Vec::new() } fn read(&self, path: &str) -> Option<Vec<u8>> { None } }
pub(crate) fn load(source: &dyn Files) -> Result<Project, Vec<Problem>> { todo!() }
""",
    "project_tests.rs": "pub fn only_in_tests() {}",
}


def surface_of(files):
    with tempfile.TemporaryDirectory() as folder:
        for name, text in files.items():
            path = Path(folder) / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        return public_surface.surface(Path(folder))


class PublicSurface(unittest.TestCase):
    def test_lists_public_items_sorted(self):
        self.assertEqual(surface_of({"lib.rs": ONE_FILE}).splitlines(), EXPECTED.splitlines())

    def test_layout_does_not_change_the_surface(self):
        self.assertEqual(surface_of(SPLIT).splitlines(), EXPECTED.splitlines())

    def test_a_changed_parameter_type_changes_the_surface(self):
        changed = ONE_FILE.replace("results: Option<&TestResults>", "results: &TestResults")
        self.assertNotEqual(surface_of({"lib.rs": changed}), surface_of({"lib.rs": ONE_FILE}))

    def test_the_same_type_name_from_another_path_changes_the_surface(self):
        one = surface_of({"lib.rs": "pub fn read(error: std::io::Error) {}"})
        other = surface_of({"lib.rs": "pub fn read(error: serde_json::Error) {}"})
        self.assertEqual(one, "fn read(std::io::Error)\n")
        self.assertEqual(other, "fn read(serde_json::Error)\n")

    def test_what_is_exported_from_another_crate_is_part_of_the_surface(self):
        exported = surface_of({"lib.rs": "mod inner;\npub use inner::Moved;\npub use crate::inner::Other;\npub use serde::{Serialize, ser::{Error as Wrong, Impossible}};\npub use toml::Value;\nuse std::fmt;"})
        self.assertEqual(exported.splitlines(), ["use serde::Serialize", "use serde::ser::Error as Wrong", "use serde::ser::Impossible", "use toml::Value"])

    def test_a_new_public_field_changes_the_surface(self):
        changed = ONE_FILE.replace("hidden: bool", "pub hidden: bool")
        self.assertNotEqual(surface_of({"lib.rs": changed}), surface_of({"lib.rs": ONE_FILE}))

    def test_test_files_are_not_part_of_the_surface(self):
        nested = {"lib.rs": ONE_FILE, "tests.rs": "pub fn a() {}", "tests/more.rs": "pub fn b() {}"}
        self.assertEqual(surface_of(nested), surface_of({"lib.rs": ONE_FILE}))

    def test_the_methods_of_a_type_of_the_crate_alone_are_not_part_of_the_surface(self):
        hasher = "pub(crate) struct Hasher(u8);\nimpl Hasher { pub fn new(label: &str) -> Hasher { todo!() } pub const SIZE: usize = 32; }\n"
        self.assertEqual(surface_of({"lib.rs": "pub fn load() {}\n" + hasher}), "fn load()\n")
        self.assertEqual(surface_of({"lib.rs": "mod ledger;\npub fn load() {}", "ledger.rs": hasher}), "fn load()\n")

    def test_the_methods_of_a_private_type_are_not_part_of_the_surface(self):
        private = "struct Node { text: String }\nimpl Node { pub fn as_str(&self) -> Option<&str> { None } }\nenum Kind { One }\nimpl Kind { pub fn name(&self) -> &str { todo!() } }\n"
        self.assertEqual(surface_of({"lib.rs": "pub fn load() {}\n" + private}), "fn load()\n")

    def test_what_a_private_type_implements_is_not_part_of_the_surface(self):
        private = "struct Node;\nimpl std::fmt::Display for Node { fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { todo!() } }\n"
        public = "pub struct Shown;\nimpl std::fmt::Display for Shown { fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { todo!() } }\n"
        self.assertEqual(surface_of({"lib.rs": private + public}).splitlines(), ["impl std::fmt::Display for Shown", "struct Shown"])

    def test_what_is_implemented_for_a_type_of_another_crate_is_part_of_the_surface(self):
        self.assertEqual(surface_of({"lib.rs": "pub trait Named { fn name(&self) -> String; }\nimpl Named for String { fn name(&self) -> String { todo!() } }"}).splitlines(), ["impl Named for String", "trait Named { fn name(&self) -> String }"])

    def test_a_public_item_of_a_module_nobody_exports_is_not_part_of_the_surface(self):
        hidden = {"lib.rs": "mod inner;\npub fn load() {}", "inner.rs": "pub fn helper() {}\npub struct Scratch { pub size: usize }\nimpl Scratch { pub fn new() -> Scratch { todo!() } }\npub const LIMIT: usize = 1;"}
        self.assertEqual(surface_of(hidden), "fn load()\n")
        inline = "mod inner { pub fn helper() {} }\npub(crate) mod shared { pub fn other() {} }\npub fn load() {}"
        self.assertEqual(surface_of({"lib.rs": inline}), "fn load()\n")

    def test_a_public_item_of_a_public_module_is_part_of_the_surface(self):
        nested = {"lib.rs": "pub mod outer;", "outer.rs": "pub mod inner;\nmod hidden;\npub fn one() {}", "outer/inner.rs": "pub fn two() {}", "outer/hidden.rs": "pub fn three() {}"}
        self.assertEqual(surface_of(nested).splitlines(), ["fn one()", "fn two()"])
        self.assertEqual(surface_of({"lib.rs": "pub mod outer { pub mod inner { pub fn two() {} } mod hidden { pub fn three() {} } }"}), "fn two()\n")

    def test_an_item_a_reachable_module_exports_by_name_is_part_of_the_surface(self):
        exported = {"lib.rs": "mod inner;\npub use inner::{helper, Renamed as Shown};\npub use crate::inner::deep::far;", "inner.rs": "pub mod deep;\npub fn helper() {}\npub fn kept() {}\npub struct Renamed;", "inner/deep.rs": "pub fn far() {}\npub fn near() {}"}
        self.assertEqual(surface_of(exported).splitlines(), ["fn far()", "fn helper()", "struct Renamed"])

    def test_an_item_with_the_name_of_a_module_is_exported_as_an_item(self):
        named = {"lib.rs": "mod load;\nmod project;\npub use project::load;", "load.rs": "pub fn inside() {}", "project.rs": "pub fn load() {}"}
        self.assertEqual(surface_of(named), "fn load()\n")

    def test_everything_of_a_module_a_reachable_module_exports_is_part_of_the_surface(self):
        everything = {"lib.rs": "mod types;\nmod other;\npub use types::*;", "types.rs": "pub struct Problem;\nimpl Problem { pub fn line(&self) -> usize { 0 } }\npub(crate) fn internal() {}", "other.rs": "pub fn unused() {}"}
        self.assertEqual(surface_of(everything).splitlines(), ["impl Problem: fn line(&self) -> usize", "struct Problem"])
        whole = {"lib.rs": "mod outer;\npub use outer::inner;", "outer.rs": "pub mod inner;\npub fn hidden() {}", "outer/inner.rs": "pub fn shown() {}"}
        self.assertEqual(surface_of(whole), "fn shown()\n")

    def test_what_a_module_nobody_reaches_exports_is_not_part_of_the_surface(self):
        chain = {"lib.rs": "mod outer;", "outer.rs": "mod inner;\npub use inner::helper;\npub use serde::Serialize;", "outer/inner.rs": "pub fn helper() {}"}
        self.assertEqual(surface_of(chain), "")
        through = {"lib.rs": "mod outer;\npub use outer::helper;", "outer.rs": "mod inner;\npub use inner::helper;", "outer/inner.rs": "pub fn helper() {}"}
        self.assertEqual(surface_of(through), "fn helper()\n")

    def test_the_methods_of_an_exported_type_are_listed_wherever_they_are_written(self):
        apart = {"lib.rs": "mod model;\nmod report;\npub use model::Project;", "model.rs": "pub struct Project;", "report.rs": "use crate::model::Project;\nimpl Project { pub fn report(&self) -> usize { 0 } }"}
        self.assertEqual(surface_of(apart).splitlines(), ["impl Project: fn report(&self) -> usize", "struct Project"])


if __name__ == "__main__":
    unittest.main()
