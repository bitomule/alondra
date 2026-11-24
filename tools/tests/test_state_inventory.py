#!/usr/bin/env python3
"""The state machine inventory matches the code, and the guard names each way of drifting from it."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

path = Path(__file__).resolve().parents[1] / "state_inventory.py"
spec = importlib.util.spec_from_file_location("state_inventory", path)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


def checkout():
    """The checkout as this test declares it: the `tree` targets in its data, read from its runfiles."""
    runfiles = os.environ.get("TEST_SRCDIR")
    if runfiles:
        return Path(runfiles) / os.environ.get("TEST_WORKSPACE", "_main")
    return Path(__file__).resolve().parents[2]


class CheckoutTest(unittest.TestCase):
    def test_the_inventory_matches_the_code(self):
        found = guard.check(checkout())
        self.assertEqual(found, [], "\n" + "\n".join(found))


MACHINE = """### M-LIFE-1 · Lifecycle

```toml state-machine
id = "M-LIFE-1"
code = ["crates/domain/src/life.rs::Life"]
truth = ["sqlite:lives"]
states = "crates/domain/src/life.rs::LifeState"
variants = ["Open", "Closed"]
terminals = ["Closed"]
```

### M-STORE-12 · Files

```toml state-machine
id = "M-STORE-12"
code = ["crates/engine-admission/src/gate.rs::save"]
```

```toml state-files
[[file]]
name = "gate.json"
writer = "crates/engine-admission/src/gate.rs"
readers = []
machine = "M-STORE-12"
```

### Timers that act on state

```toml timers
[[timer]]
name = "IN_FLIGHT"
where = "crates/engine-admission/src/gate.rs"
decides = "state"
machine = "M-STORE-12"
triggers = "frees a slot"
```
"""

LIFE = """pub struct Life;
pub enum LifeState {
    #[serde(rename = "open")]
    Open,
    Closed { at: u64 },
}
"""

GATE = """use std::time::Duration;
const IN_FLIGHT: Duration = Duration::from_secs(180);
const FILE: &str = "gate.json";
pub fn save() {}
"""

SCHEMA = "CREATE TABLE lives(id TEXT PRIMARY KEY);\n"


class GuardTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.write("docs/architecture/state-machines.md", MACHINE)
        self.write("crates/domain/src/life.rs", LIFE)
        self.write("crates/engine-admission/src/gate.rs", GATE)
        self.write("crates/store-sqlite/src/schema.sql", SCHEMA)

    def write(self, relative, text):
        target = self.root / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def edit(self, relative, old, new):
        target = self.root / relative
        text = target.read_text()
        self.assertIn(old, text)
        target.write_text(text.replace(old, new))

    def problems(self):
        return guard.check(self.root)

    def assertOne(self, needle):
        found = self.problems()
        self.assertEqual(len(found), 1, "\n".join(found))
        self.assertIn(needle, found[0])

    def test_the_clean_fixture_passes(self):
        self.assertEqual(self.problems(), [])

    def test_a_section_without_its_block_is_named(self):
        self.edit("docs/architecture/state-machines.md", "### Timers", "### M-LIFE-2 · New one\n\nProse.\n\n### Timers")
        self.assertOne("M-LIFE-2 has no state-machine block")

    def test_a_block_outside_its_section_is_named(self):
        self.edit("docs/architecture/state-machines.md", 'id = "M-STORE-12"', 'id = "M-LIFE-9"')
        found = self.problems()
        self.assertTrue(any("sits in section M-STORE-12" in line for line in found), found)

    def test_an_unreadable_block_is_named(self):
        self.edit("docs/architecture/state-machines.md", 'truth = ["sqlite:lives"]', "truth = [sqlite:lives]")
        found = self.problems()
        self.assertTrue(any("unreadable state-machine block" in line for line in found), found)

    def test_a_symbol_that_no_longer_exists_is_named(self):
        self.edit("crates/domain/src/life.rs", "pub struct Life;", "pub struct Living;")
        self.assertOne("crates/domain/src/life.rs::Life is not defined")

    def test_a_new_state_without_its_declaration_is_named(self):
        self.edit("crates/domain/src/life.rs", "    Open,", "    Open,\n    Parked(u8),")
        self.assertOne("['Parked'] undeclared")

    def test_a_declared_state_the_code_lost_is_named(self):
        self.edit("crates/domain/src/life.rs", "    Open,\n", "")
        self.assertOne("['Open'] declared but gone")

    def test_an_undeclared_state_file_is_named(self):
        self.write("crates/engine-sdk/src/hold.rs", 'const F: &str = "hold.json";\n')
        self.assertOne("state file hold.json is not in the state-files block")

    def test_a_second_module_naming_a_state_file_is_named(self):
        self.write("crates/engine-sdk/src/also.rs", 'fn f() { write("gate.json"); }\n')
        self.assertOne("crates/engine-sdk/src/also.rs: names gate.json")

    def test_a_state_file_with_two_writers_is_named(self):
        self.edit("docs/architecture/state-machines.md", 'writer = "crates/engine-admission/src/gate.rs"',
                  'writer = ["crates/engine-admission/src/gate.rs", "crates/engine-sdk/src/also.rs"]')
        self.assertOne("gate.json has 2 writers")

    def test_a_state_file_named_only_in_a_test_or_a_comment_passes(self):
        self.write("crates/engine-sdk/src/hold.rs", '// see "hold.json"\nfn f() {}\n#[cfg(test)]\nmod tests { const F: &str = "hold.json"; }\n')
        self.write("crates/engine-sdk/src/hold_tests.rs", 'const F: &str = "hold.json";\n')
        self.assertEqual(self.problems(), [])

    def test_code_outside_src_is_not_production(self):
        self.write("crates/engine-sdk/model/fake.rs", 'enum HostPhase { A }\nconst F: &str = "gate.json";\n')
        self.assertEqual(self.problems(), [])

    def test_a_table_that_is_the_truth_of_no_machine_is_named(self):
        self.write("crates/store-sqlite/src/schema.sql", SCHEMA + "CREATE TABLE IF NOT EXISTS holds(id TEXT);\n")
        self.assertOne("table holds is the truth of no machine")

    def test_sql_writing_another_crates_table_is_named(self):
        self.write("crates/engine-sdk/src/w.rs", 'fn f(c: C) { c.execute("UPDATE lives SET id=1", []); }\n')
        self.assertOne("writes table lives, which ['store-sqlite'] owns")
        self.write("crates/engine-sdk/src/w.rs", 'fn f() {}\n#[cfg(test)]\nmod tests { fn t(c: C) { c.execute("UPDATE lives SET id=1", []); } }\n')
        self.assertEqual(self.problems(), [])

    def test_an_undeclared_time_constant_is_named(self):
        self.edit("crates/engine-admission/src/gate.rs", "pub fn save", "const RETRY_AFTER_MS: u64 = 500;\npub fn save")
        self.assertOne("time constant RETRY_AFTER_MS is not in the timers block")

    def test_a_timer_without_its_decision_is_named(self):
        self.edit("docs/architecture/state-machines.md", 'decides = "state"', 'decides = "maybe"')
        self.assertOne('needs decides = "state" or "pace"')

    def test_a_declared_timer_the_code_lost_is_named(self):
        self.edit("crates/engine-admission/src/gate.rs", "const IN_FLIGHT: Duration = Duration::from_secs(180);\n", "")
        self.assertOne("timer IN_FLIGHT is declared")

    def test_a_state_enum_of_no_machine_is_named(self):
        self.write("crates/domain-rules/src/x.rs", "pub(crate) enum HoldPhase { A, B }\n")
        self.assertOne("enum HoldPhase is part of no machine")

    def test_an_exemption_with_its_reason_passes(self):
        self.write("crates/domain-rules/src/x.rs", "enum ReportStatus { A }\n")
        self.write("tools/state_inventory_exempt.tsv", "enum\tcrates/domain-rules/src/x.rs::ReportStatus\tonly formats a report\n")
        self.assertEqual(self.problems(), [])

    def test_a_known_defect_passes_until_it_is_fixed_and_then_must_go(self):
        self.write("crates/engine-sdk/src/also.rs", 'fn f() { write("gate.json"); }\n')
        self.write("tools/state_inventory_known.tsv", "owner\tgate.json crates/engine-sdk/src/also.rs\tF-STORE-99\n")
        self.assertEqual(self.problems(), [])
        (self.root / "crates/engine-sdk/src/also.rs").unlink()
        self.assertOne("(F-STORE-99) no longer fails: remove it")


if __name__ == "__main__":
    unittest.main()
