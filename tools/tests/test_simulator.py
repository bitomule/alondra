#!/usr/bin/env python3
"""A runtime must support the requested phone before any baseline is recorded."""
import importlib.util
import os
from pathlib import Path
import unittest

ROOT = Path(os.environ["TEST_SRCDIR"]) / os.environ["TEST_WORKSPACE"]
spec = importlib.util.spec_from_file_location("simulator", ROOT / "tools/simulator.py")
simulator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(simulator)


class SimulatorContractTests(unittest.TestCase):
    def test_a_runtime_with_the_expected_phone_can_be_used(self):
        expected = simulator.config()
        runtime = {"identifier": expected["runtime_id"], "version": expected["runtime"],
                   "isAvailable": True, "buildversion": expected["runtime_build"],
                   "supportedDeviceTypes": [{"identifier": expected["device_id"]}]}
        self.assertEqual(simulator.compatible({"runtimes": [runtime]}, expected),
                         {"runtime": expected["runtime_id"], "build": expected["runtime_build"], "device": expected["device"]})
        for change in ({"isAvailable": False}, {"version": "27.1"}, {"buildversion": "another-build"},
                       {"supportedDeviceTypes": [{"identifier": "com.apple.CoreSimulator.SimDeviceType.iPhone-Duo"}]}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                simulator.compatible({"runtimes": [runtime | change]}, expected)
        with self.assertRaises(ValueError):
            simulator.compatible({"runtimes": []}, expected)


if __name__ == "__main__":
    unittest.main()
