#!/usr/bin/env python3
"""Check the snapshot simulator contract before recording or running visual tests."""
import argparse
import ast
import json
from pathlib import Path
import plistlib
import subprocess


def config():
    path = Path(__file__).resolve().parents[1] / "apple/Testing/Simulator/config.bzl"
    module = ast.parse(path.read_text())
    if len(module.body) != 1 or not isinstance(module.body[0], ast.Assign):
        raise ValueError("The simulator config must be one literal assignment")
    return ast.literal_eval(module.body[0].value)


def compatible(inventory, expected):
    for runtime in inventory["runtimes"]:
        if runtime["identifier"] != expected["runtime_id"]:
            continue
        if (not runtime.get("isAvailable") or runtime["version"] != expected["runtime"]
                or runtime["buildversion"] != expected["runtime_build"]):
            break
        if any(device["identifier"] == expected["device_id"] for device in runtime.get("supportedDeviceTypes", [])):
            return {"runtime": runtime["identifier"], "build": runtime["buildversion"], "device": expected["device"]}
    raise ValueError(f"Required simulator unavailable or incompatible: {expected['device']} on iOS {expected['runtime']}")


def probe(device_set):
    expected = config()
    xcodebuild = Path(subprocess.check_output(["xcrun", "--find", "xcodebuild"], text=True).strip())
    version = plistlib.loads((xcodebuild.parents[3] / "version.plist").read_bytes())
    print(json.dumps(version, indent=2))
    if version["ProductBuildVersion"] != expected["xcode_build"]:
        raise ValueError(f"Expected Xcode build {expected['xcode_build']}; select the canonical toolchain")
    inventory = json.loads(subprocess.check_output(["xcrun", "simctl", "list", "runtimes", "-j"], text=True))
    print(json.dumps(inventory, indent=2))
    selected = compatible(inventory, expected)
    device_set.mkdir(parents=True, exist_ok=True)
    command = ["xcrun", "simctl", "--set", str(device_set.resolve())]
    device = subprocess.check_output([*command, "create", "snapshot-compatibility-probe",
                                      expected["device_id"], expected["runtime_id"]], text=True).strip()
    try:
        subprocess.run([*command, "boot", device], check=True, timeout=120)
        subprocess.run([*command, "bootstatus", device, "-b"], check=True, timeout=180)
    finally:
        subprocess.run([*command, "shutdown", device], capture_output=True)
        subprocess.run([*command, "delete", device], check=True, timeout=60)
    print(json.dumps(selected))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--device-set", type=Path, default=Path(".scratch/simulator-probe"))
    probe(parser.parse_args().device_set)
