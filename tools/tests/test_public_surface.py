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


if __name__ == "__main__":
    unittest.main()
