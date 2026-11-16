#!/usr/bin/env python3
"""No crate's main cycle of production modules grows past its limit in `tools/module_cycles.tsv`,
and the module graph counts only what the library compiles outside its tests."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

path = Path(__file__).resolve().parents[1] / "module_graph.py"
spec = importlib.util.spec_from_file_location("module_graph", path)
graph = importlib.util.module_from_spec(spec)
spec.loader.exec_module(graph)


def checkout():
    """The checkout as this test declares it: the `tree` targets in its data, read from its runfiles."""
    runfiles = os.environ.get("TEST_SRCDIR")
    if runfiles:
        return Path(runfiles) / os.environ.get("TEST_WORKSPACE", "_main")
    return Path(__file__).resolve().parents[2]


def limits(root):
    rows = (root / "tools" / "module_cycles.tsv").read_text().splitlines()
    return [(source, int(most)) for source, most in (row.split("\t") for row in rows if row and not row.startswith("#"))]


class CheckoutTest(unittest.TestCase):
    def test_no_main_cycle_grows_past_its_limit(self):
        root = checkout()
        for source, most in limits(root):
            with self.subTest(source):
                declared = graph.MOD.findall(graph.without_tests(graph.code_only((root / source / "lib.rs").read_text())))
                self.assertGreaterEqual(len(graph.production_modules(root / source)), len(declared) // 2,
                    f"{source}: most modules lib.rs declares were not read, so no cycle could be measured")
                cycle, lines = graph.main_cycle(root / source)
                self.assertLessEqual(len(cycle), most, f"{source}: {len(cycle)} modules ({lines} lines) depend on each other: {' '.join(cycle)}")


class GraphTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.source = Path(directory.name)

    def crate(self, files):
        for name, text in files.items():
            (self.source / name).parent.mkdir(parents=True, exist_ok=True)
            (self.source / name).write_text(text)

    def test_two_modules_that_use_each_other_are_a_cycle(self):
        self.crate({
            "lib.rs": "mod a;\nmod b;\nmod c;\n",
            "a.rs": "use crate::b::B;\n",
            "b.rs": "fn f() { crate::a::g(); }\n",
            "c.rs": "use crate::a;\n",
        })
        self.assertEqual(graph.main_cycle(self.source)[0], ["a", "b"])

    def test_sources_linked_from_elsewhere_as_in_bazels_runfiles_still_count(self):
        self.crate({
            "real/lib.rs": "mod a;\nmod b;\n",
            "real/a.rs": "use crate::b::B;\n",
            "real/b.rs": "fn f() { crate::a::g(); }\n",
        })
        linked = self.source / "runfiles"
        linked.mkdir()
        for name in ("lib.rs", "a.rs", "b.rs"):
            (linked / name).symlink_to(self.source / "real" / name)
        self.assertEqual(graph.main_cycle(linked)[0], ["a", "b"])

    def test_test_code_comments_and_strings_never_make_a_cycle(self):
        self.crate({
            "lib.rs": "mod a;\nmod b;\n#[cfg(test)]\nmod fixtures;\n",
            "a.rs": "use crate::b;\n",
            "b.rs": (
                "// crate::a\n/* crate::a { */\nconst S: &str = \"crate::a {\";\nconst R: &str = r#\"crate::a\"#;\n"
                "const C: char = '{';\nfn f<'a>(x: &'a str) {}\n"
                "#[cfg(test)]\nmod tests { use crate::a; fn g() { let _ = '}'; } }\n"
                "#[cfg(test)]\nmod more;\n"
            ),
            "b/more.rs": "use crate::a;\n",
            "fixtures.rs": "use crate::a; use crate::b;\n",
        })
        self.assertEqual(graph.main_cycle(self.source), ([], 0))
        self.assertNotIn("fixtures", graph.production_modules(self.source))

    def test_a_production_module_declared_in_a_directory_counts(self):
        self.crate({
            "lib.rs": "pub mod a;\npub(crate) mod b;\n",
            "a.rs": "mod inner;\n",
            "a/inner.rs": "use crate::b;\n",
            "b/mod.rs": "#[path = \"other.rs\"]\nmod named;\n",
            "b/other.rs": "use crate::a;\n",
        })
        self.assertEqual(graph.main_cycle(self.source)[0], ["a", "b"])


if __name__ == "__main__":
    unittest.main()
