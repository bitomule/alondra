#!/usr/bin/env python3
"""The summary a person reads from shared connection measurements, from a sample file."""
import importlib.util
import io
import json
import os
from contextlib import redirect_stdout
from pathlib import Path
import tempfile
import unittest

module_path = Path(__file__).resolve().parents[1] / "connection_measurements.py"
spec = importlib.util.spec_from_file_location("connection_measurements", module_path)
measurements = importlib.util.module_from_spec(spec)
spec.loader.exec_module(measurements)


def row(ms, route="relay", network="cellular", radio="NR", outcome="ok", kind="views", at="2026-10-15T10:00:00Z"):
    line = {"at": at, "mac": "a1b2c3", "operation": "perform", "method": "GET", "kind": kind,
            "ms": ms, "network": network, "outcome": outcome}
    if route:
        line["route"] = route
    if radio:
        line["radio"] = radio
    return line


class ConnectionMeasurementsTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(dir=os.environ.get("TEST_TMPDIR"))
        self.addCleanup(self.directory.cleanup)

    def write(self, rows):
        path = Path(self.directory.name) / "connection-measurements.jsonl"
        path.write_text("".join(json.dumps(r) + "\n" for r in rows) + "not json\n", encoding="utf-8")
        return str(path)

    def test_groups_by_path_network_and_radio_with_waits_apart(self):
        path = self.write([row(ms) for ms in (40, 50, 60, 70, 500)]
                          + [row(9, route="localNetwork", network="wifi", radio=None)]
                          + [row(0, route=None, outcome="failed")]
                          + [row(25000, kind="changes")]
                          + [row(1, outcome="cancelled")])
        summary = measurements.summarize(measurements.load([path]))
        self.assertEqual([(g["waits"], g["route"], g["network"]) for g in summary],
                         [(False, "relay", "cellular"), (False, "localNetwork", "wifi"),
                          (False, "none", "cellular"), (True, "relay", "cellular")])
        relay = summary[0]
        self.assertEqual((relay["count"], relay["failed"], relay["median_ms"], relay["p90_ms"]), (5, 0, 60, 500))
        self.assertEqual(summary[2]["failed"], 1)
        self.assertIsNone(summary[2]["median_ms"])

    def test_keeps_only_the_dates_asked_for(self):
        path = self.write([row(10, at="2026-10-14T23:59:59Z"), row(20, at="2026-10-15T08:00:00Z"),
                           row(30, at="2026-10-16T00:00:00Z")])
        rows = measurements.load([path], measurements.parse_time("2026-10-15"), measurements.parse_time("2026-10-16"))
        self.assertEqual([r["ms"] for r in rows], [20])

    def test_prints_a_table(self):
        path = self.write([row(80), row(120, outcome="mac_error")])
        out = io.StringIO()
        with redirect_stdout(out):
            measurements.main([path])
        lines = out.getvalue().splitlines()
        self.assertEqual(lines[0].split(), ["route", "network", "radio", "requests", "failed", "median", "ms", "p90", "ms"])
        self.assertEqual(lines[1].split(), ["relay", "cellular", "NR", "2", "0", "80", "120"])


if __name__ == "__main__":
    unittest.main()
