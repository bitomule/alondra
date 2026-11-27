#!/usr/bin/env python3
"""A test runner's `create_simulator_action`: a simulator no other run can reach, printed as its UDID.

rules_apple's own creator looks the device up by `BAZEL_TEST_<type>_<version>` whenever it is
called without `--no-reuse-simulator`, and the runner never passes it: every run of every checkout
then shares one device, and the first run to finish deletes it under the others (`No matching
device in set`, `Mach error -308`). This one always creates a device named after a fresh random id,
and removes it again when it cannot boot it, so the runner's clean-up (which deletes the UDID it
was given) is the only other thing that ever touches it.
"""
import json
import os
import subprocess
import sys
import uuid


def simctl(*arguments, check=True):
    return subprocess.run(
        ["xcrun", "simctl", *arguments], check=check, text=True, stdout=subprocess.PIPE
    ).stdout


def runtime_identifier(os_version, run=simctl):
    runtimes = json.loads(run("list", "runtimes", "-j"))["runtimes"]
    for runtime in runtimes:
        if runtime["platform"] == "iOS" and runtime["isAvailable"] and runtime["version"] == os_version:
            return runtime["identifier"]
    raise RuntimeError(f"no available iOS {os_version} runtime")


def create(device_type, os_version, run=simctl, token=lambda: uuid.uuid4().hex[:12]):
    runtime = runtime_identifier(os_version, run)
    name = f"BAZEL_TEST_{device_type}_{os_version}_{token()}"
    udid = run("create", name, device_type, runtime).strip()
    print(f"Created simulator '{name}' ({udid})", file=sys.stderr)
    try:
        # `bootstatus -b` boots the device and returns once the system has finished starting.
        print(run("bootstatus", udid, "-b"), file=sys.stderr)
    except BaseException:
        run("delete", udid, check=False)
        raise
    return udid


def main(environment=os.environ):
    print(create(environment["SIMULATOR_DEVICE_TYPE"], environment["SIMULATOR_OS_VERSION"]))


if __name__ == "__main__":
    main()
