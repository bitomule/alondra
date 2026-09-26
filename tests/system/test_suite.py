import copy
import unittest
import suite


def fixture(commit="before"):
    group = {"latency_cohorts": [{"cohort": "accepted", "p95_ms": 10, "p99_ms": 12, "count": 100}],
             "offered_requests":100, "accounts": {"0": [10, 0, 0, 0]}, "overlapping_successful_idle_holds": 64}
    case = {"phase": "load", "profile": {"gate": "sustained"}, "accounts": 16, "macs": 32, "links": 64,
            "polls": copy.deepcopy(group), "bursts": copy.deepcopy(group), "revocation_under_load_seconds": 1}
    sample = {"phase": "idle_before_restart", "cpu_percent": "2%", "process": {"VmRSS": "10000 kB"}, "open_fds": 20}
    return {"status": "passed", "source": {"commit": commit, "dirty": False}, "management": {"enabled": True},
            "versions": {"postgres":"17.11", "keycloak":"26.7.4", "rust_builder":"1.96", "dockerfile_sha256":"synthetic-file", "cargo_lock_sha256":"before"},
            "hardware": {"host_model": "synthetic-test-machine"}, "load_configuration": {"limits": "baseline", "duration_seconds": 600},
            "cases": [case], "resources": {"limits": {"relay_cpus": 1}, "observations": [sample]}}


class ComparatorTests(unittest.TestCase):
    def test_different_commits_same_hardware_are_comparable(self):
        result = suite.compare(fixture(), fixture("after"))
        self.assertEqual(result["regressions"], 0)
        self.assertEqual(result["candidate_commit"], "after")

    def test_different_hardware_or_limits_cannot_hide_regression(self):
        for field in ("hardware", "load_configuration"):
            changed = fixture()
            changed[field]["different"] = True
            with self.assertRaises(ValueError):
                suite.compare(fixture(), changed)

    def test_accepted_tail_regression_is_detected(self):
        changed = fixture("after")
        changed["cases"][0]["bursts"]["latency_cohorts"][0]["p95_ms"] = 100
        result = suite.compare(fixture(), changed)
        self.assertEqual(result["regressions"], 1)
        self.assertTrue(next(row for row in result["changes"] if row["metric"] == "short_accepted_p95_ms")["regression"])

    def test_admission_collapse_and_overload_are_regressions(self):
        changed = fixture("after")
        changed["cases"][0]["bursts"]["latency_cohorts"] = [
            {"cohort":"accepted", "p95_ms":1, "p99_ms":1, "count":10},
            {"cohort":"expected_overload", "p95_ms":1, "p99_ms":1, "count":90}]
        changed["cases"][0]["polls"]["overlapping_successful_idle_holds"] = 10
        self.assertGreaterEqual(suite.compare(fixture(),changed)["regressions"],3)

    def test_environment_change_rejected_but_cargo_lock_change_is_evaluated(self):
        changed = fixture("after")
        changed["versions"]["keycloak"] = "different"
        with self.assertRaises(ValueError):
            suite.compare(fixture(),changed)
        changed = fixture("after")
        changed["versions"]["cargo_lock_sha256"] = "after"
        self.assertEqual(suite.compare(fixture(),changed)["regressions"],0)

    def test_smoke_dirty_or_post_restart_only_is_not_a_baseline(self):
        for mutation in (lambda x: x["source"].update(dirty=True),
                         lambda x: x["cases"][0]["profile"].update(gate="smoke_only"),
                         lambda x: x["resources"]["observations"][0].update(phase="post_restart")):
            changed = fixture()
            mutation(changed)
            with self.assertRaises(ValueError):
                suite.compare(fixture(), changed)
