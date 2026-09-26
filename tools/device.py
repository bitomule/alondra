#!/usr/bin/env python3
"""Build, install and launch the development app on an explicitly named iPhone."""
import os
from pathlib import Path
import plistlib
import shlex
import subprocess
import sys
import zipfile


def main():
    os.chdir(Path(__file__).resolve().parents[1])
    device = os.environ.get("DEVICE", "David iPhone")
    if not device.strip():
        raise ValueError("Set DEVICE to a device name or UDID")
    # Let CoreDevice resolve the name and reject ambiguous/unreachable devices
    # before spending time compiling. Never choose the first phone in a list.
    subprocess.run(["xcrun", "devicectl", "device", "info", "details",
                    "--device", device], check=True)
    bazel = ["bazelisk", *shlex.split(os.environ.get("BAZEL_STARTUP_FLAGS", ""))]
    flags = ["--ios_multi_cpus=arm64", "--jobs=1"]
    target = "//apple:AlondraiOS"
    subprocess.run([*bazel, "build", target, *flags], check=True)
    output = subprocess.check_output(
        [*bazel, "cquery", target, *flags, "--output=files"], text=True)
    archives = [Path(line) for line in output.splitlines() if line.endswith(".ipa")]
    if len(archives) != 1 or not archives[0].is_file():
        raise ValueError("Expected exactly one built IPA")
    with zipfile.ZipFile(archives[0]) as archive:
        info = plistlib.loads(archive.read("Payload/Alondra.app/Info.plist"))
    if info.get("DTPlatformName") != "iphoneos":
        raise ValueError("Refusing a simulator artifact")
    bundle = info["CFBundleIdentifier"]
    subprocess.run(["xcrun", "devicectl", "device", "install", "app",
                    "--device", device, str(archives[0])], check=True)
    subprocess.run(["xcrun", "devicectl", "device", "process", "launch",
                    "--device", device, "--terminate-existing", bundle], check=True)
    print(f"Installed and launched {bundle} on {device}.")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, zipfile.BadZipFile,
            plistlib.InvalidFileException, subprocess.CalledProcessError) as error:
        print(f"Device installation failed: {error}", file=sys.stderr)
        sys.exit(1)
