#!/usr/bin/env python3
"""Every call of the simulator creator makes its own device and never takes one that exists."""
import json
from pathlib import Path
import subprocess
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import create_simulator


class FakeSimctl:
    def __init__(self, boot_fails=False):
        self.devices = {}
        self.calls = []
        self.boot_fails = boot_fails

    def __call__(self, *arguments, check=True):
        self.calls.append(arguments)
        if arguments == ("list", "runtimes", "-j"):
            return json.dumps({"runtimes": [
                {"platform": "iOS", "isAvailable": True, "version": "26.3.1", "identifier": "runtime.ios.26.3.1"},
                {"platform": "iOS", "isAvailable": True, "version": "26.4", "identifier": "runtime.ios.26.4"},
            ]})
        if arguments[0] == "create":
            udid = f"udid-{len(self.devices) + 1}"
            self.devices[udid] = arguments[1]
            return udid + "\n"
        if arguments[0] == "bootstatus":
            if self.boot_fails:
                raise subprocess.CalledProcessError(164, "simctl")
            return "booted"
        if arguments[0] == "delete":
            self.devices.pop(arguments[1], None)
            return ""
        raise AssertionError(arguments)


class CreateSimulatorTests(unittest.TestCase):
    def test_two_runs_get_different_devices_even_when_the_first_is_still_there(self):
        simctl = FakeSimctl()
        first = create_simulator.create("iPhone 17 Pro", "26.3.1", run=simctl)
        second = create_simulator.create("iPhone 17 Pro", "26.3.1", run=simctl)
        self.assertNotEqual(first, second)
        self.assertEqual(len(set(simctl.devices.values())), 2)
        self.assertEqual(len(simctl.devices), 2)

    def test_it_never_looks_an_existing_device_up(self):
        simctl = FakeSimctl()
        create_simulator.create("iPhone 17 Pro", "26.3.1", run=simctl)
        self.assertNotIn(("list", "devices", "-j"), simctl.calls)

    def test_the_device_uses_the_requested_runtime(self):
        simctl = FakeSimctl()
        create_simulator.create("iPhone 17 Pro", "26.3.1", run=simctl)
        self.assertIn(("create", "BAZEL_TEST_iPhone 17 Pro_26.3.1_" + simctl.devices["udid-1"].rsplit("_", 1)[1],
                       "iPhone 17 Pro", "runtime.ios.26.3.1"), simctl.calls)

    def test_a_device_that_does_not_boot_is_deleted_and_the_failure_stays(self):
        simctl = FakeSimctl(boot_fails=True)
        with self.assertRaises(subprocess.CalledProcessError):
            create_simulator.create("iPhone 17 Pro", "26.3.1", run=simctl)
        self.assertEqual(simctl.devices, {})

    def test_an_unavailable_runtime_is_an_error(self):
        with self.assertRaises(RuntimeError):
            create_simulator.create("iPhone 17 Pro", "99.0", run=FakeSimctl())


if __name__ == "__main__":
    unittest.main()
