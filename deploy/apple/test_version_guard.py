#!/usr/bin/env python3
"""Version guard against recorded App Store Connect responses; no network."""
import json
import subprocess
import unittest
from unittest import mock

from version_guard import GuardError, check, fetch, parse_version, uploaded


def response(*builds):
    data, included = [], []
    for index, (version, number) in enumerate(builds):
        data.append({"type": "builds", "id": "b%d" % index, "attributes": {"version": str(number)},
                     "relationships": {"preReleaseVersion": {"data": {"type": "preReleaseVersions", "id": "v" + version}}}})
        included.append({"type": "preReleaseVersions", "id": "v" + version, "attributes": {"version": version}})
    return json.dumps({"data": data, "included": included}) + '\n{"meta":{}}\n'


# iOS builds 36-38 as App Store Connect held them when the version went backwards.
REGRESSED = uploaded(response(("0.2.49", 36), ("0.1.1", 37), ("0.1.2", 38)))


class VersionGuardTests(unittest.TestCase):
    def test_reads_marketing_version_and_build_number_from_asc(self):
        self.assertEqual(sorted(REGRESSED), [((0, 1, 1), 37), ((0, 1, 2), 38), ((0, 2, 49), 36)])

    def test_refuses_the_version_that_went_backwards(self):
        self.assertIn("0.1.2 is lower than 0.2.49", check(parse_version("0.1.2"), 39, REGRESSED))

    def test_accepts_a_higher_version(self):
        self.assertIsNone(check(parse_version("0.3.0"), 39, REGRESSED))

    def test_compares_numerically_not_as_text(self):
        self.assertIsNone(check(parse_version("0.2.100"), 40, REGRESSED))
        self.assertIsNotNone(check(parse_version("0.2.9"), 40, REGRESSED))

    def test_same_version_needs_a_higher_build(self):
        self.assertIn("not above build 36", check(parse_version("0.2.49"), 36, REGRESSED))
        self.assertIsNone(check(parse_version("0.2.49"), 37, REGRESSED))

    def test_app_without_builds_passes(self):
        self.assertIsNone(check(parse_version("0.0.1"), 1, []))

    def test_non_json_answer_is_refused(self):
        with self.assertRaises(GuardError):
            uploaded("missing authentication")

    def test_asc_failure_is_refused_even_with_json_on_stdout(self):
        failed = subprocess.CompletedProcess([], 1, stdout=response(("0.1.0", 1)), stderr="missing authentication")
        with mock.patch("version_guard.subprocess.run", return_value=failed):
            with self.assertRaisesRegex(GuardError, "exit 1"):
                fetch("6816262826")
        succeeded = subprocess.CompletedProcess([], 0, stdout=response(("0.1.0", 1)), stderr="")
        with mock.patch("version_guard.subprocess.run", return_value=succeeded):
            self.assertEqual(fetch("6816262826"), [((0, 1, 0), 1)])


if __name__ == "__main__":
    unittest.main()
