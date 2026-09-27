#!/usr/bin/env python3
"""Opt-in native client scenarios. No XCUITest, personal sessions or real HTTP peers."""
from __future__ import annotations

import argparse
import hashlib
import fcntl
import json
import math
import os
from pathlib import Path
import plistlib
import shutil
import signal
import stat
import subprocess
import sys
import time
import uuid
import zipfile

ROOT = Path(__file__).resolve().parents[2]
BUNDLE = "com.bitomule.alondra.nativeharness"
CHECKS = {
    "large-overview": {"exact_large_unicode_document", "all_fragments_requested", "snapshot_released",
                       "verified_encrypted_requests", "single_token_exchange"},
    "session-revoked": {"revoked_read_rejected", "first_fragment_reached", "no_token_renewal_after_revocation",
                        "issuer_revocation_executed", "credential_removed", "cached_lease_rejected"},
    "lost-response": {"receipt_recovered", "durable_result_after_restart", "mutation_submitted_once",
                      "status_read_after_loss", "verified_encrypted_request"},
}


def validate_report(report: dict, run_id: str, scenario: str) -> None:
    if report.get("schema") != 1 or report.get("scope") != "native-client-with-synthetic-http-peers":
        raise ValueError("wrong report contract")
    if report.get("runID") != run_id or report.get("scenario") != scenario:
        raise ValueError("report belongs to another run or scenario")
    if report.get("status") != "passed" or report.get("error") is not None:
        raise ValueError("scenario failed or incomplete")
    checks = report.get("checks")
    if not isinstance(checks, dict) or set(checks) != CHECKS[scenario] or any(v is not True for v in checks.values()):
        raise ValueError("missing, unexpected or failed behavioral assertions")
    elapsed = report.get("elapsedMilliseconds")
    if type(elapsed) not in (int, float) or not math.isfinite(elapsed) or elapsed < 0:
        raise ValueError("invalid elapsed time")


def hash_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def atomic_json(path: Path, value: dict) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    temporary.replace(path)


def base_environment() -> dict[str, str]:
    # Do not forward OP_*, credentials or launch overrides into the test app.
    keys = ("PATH", "HOME", "TMPDIR", "DEVELOPER_DIR", "LANG", "LC_ALL")
    return {key: os.environ[key] for key in keys if key in os.environ}


class Commands:
    def __init__(self, output: Path):
        self.output = output
        self.index = 0

    def run(self, args: list[str], *, timeout: float = 60, env: dict | None = None) -> str:
        self.index += 1
        log = self.output / f"command-{self.index:03}.log"
        with log.open("w") as stream:
            # Paths/flags contain only synthetic fixture metadata, never credentials.
            stream.write(json.dumps(args) + "\n")
            stream.flush()
            process = subprocess.Popen(args, cwd=ROOT, env=env or base_environment(),
                                       stdout=stream, stderr=subprocess.STDOUT, text=True, start_new_session=True)
            try:
                process.wait(timeout=timeout)
            except BaseException:
                # Own the entire launched process group, never grep/kill other builds.
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                    process.wait(timeout=5)
                except (ProcessLookupError, subprocess.TimeoutExpired):
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    process.wait()
                raise
        if process.returncode:
            raise RuntimeError(f"command failed ({process.returncode}); see {log.name}")
        return "\n".join(log.read_text().splitlines()[1:]).strip()


def interrupted(signum, frame):
    raise InterruptedError("native harness interrupted by signal " + str(signum))


def cleanup_owned(commands: Commands, owned_udid: str | None, cache: Path, cache_lock, fresh: bool) -> list[str]:
    errors = []
    if owned_udid:
        try:
            commands.run(["xcrun", "simctl", "delete", owned_udid])
        except (OSError, RuntimeError, subprocess.SubprocessError):
            errors.append("simulator_cleanup_failed")
    if cache_lock is not None:
        try:
            commands.run(cache_startup(cache) + ["shutdown"], timeout=60)
            if fresh:
                shutil.rmtree(cache)
        except (OSError, RuntimeError, subprocess.SubprocessError):
            errors.append("build_cache_cleanup_failed")
        finally:
            cache_lock.close()
    return errors



def extract_app(archive: Path, destination: Path, platform: str = "ios") -> Path:
    with zipfile.ZipFile(archive) as zipped:
        for item in zipped.infolist():
            relative = Path(item.filename)
            if relative.is_absolute() or ".." in relative.parts:
                raise ValueError("archive path escapes extraction directory")
            mode = item.external_attr >> 16
            if stat.S_ISLNK(mode):
                raise ValueError("harness archives must not contain symlinks")
            target = destination / relative
            if item.is_dir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with zipped.open(item) as source, target.open("wb") as output:
                    shutil.copyfileobj(source, output)
                target.chmod(mode & 0o777 or 0o600)
    apps = list(destination.glob("Payload/*.app" if platform == "ios" else "*.app"))
    if len(apps) != 1:
        raise ValueError("expected exactly one app in harness archive")
    return apps[0]


def app_executable(app: Path, platform: str) -> Path:
    info = app / ("Contents/Info.plist" if platform == "mac" else "Info.plist")
    plist = plistlib.loads(info.read_bytes())
    if plist.get("CFBundleIdentifier") != BUNDLE:
        raise ValueError("only the dedicated synthetic harness bundle is allowed")
    name = plist.get("CFBundleExecutable")
    if not isinstance(name, str) or Path(name).name != name:
        raise ValueError("invalid executable name")
    executable = app / ("Contents/MacOS" if platform == "mac" else "") / name
    if executable.is_symlink() or not executable.is_file():
        raise ValueError("missing harness executable")
    return executable


def cache_startup(cache: Path) -> list[str]:
    return ["bazelisk", "--nohome_rc", "--nosystem_rc", "--output_user_root=" + str(cache / "user-root"),
            "--output_base=" + str(cache / "output-base")]


def acquire_cache(cache: Path):
    if cache.is_symlink():
        raise ValueError("build cache must not be a symlink")
    if cache.exists():
        marker = cache / "native-harness-cache.json"
        if not marker.is_file() or json.loads(marker.read_text()) != {"schema": 1, "owner": "alondra-native-harness"}:
            raise ValueError("refusing to reuse an unowned cache; choose a new dedicated directory")
    else:
        cache.mkdir(parents=True, mode=0o700)
        atomic_json(cache / "native-harness-cache.json", {"schema": 1, "owner": "alondra-native-harness"})
    handle = (cache / "build.lock").open("a")
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        handle.close()
        raise RuntimeError("native harness build cache is already in use") from None
    return handle


def free_bytes(path: Path) -> int:
    while not path.exists():
        path = path.parent
    return shutil.disk_usage(path).free


def build_app(commands: Commands, output: Path, cache: Path, platform: str) -> Path:
    target = "AlondraNativeHarnessiOS" if platform == "ios" else "AlondraNativeHarnessMac"
    label = "//apple/NativeHarness:" + target
    # Dedicated locked cache; never read user BES/upload policy.
    startup = cache_startup(cache)
    flags = ["-c", "dbg", "--jobs=1", "--local_resources=cpu=1", "--remote_cache=", "--bes_backend=", "--lockfile_mode=error"]
    if platform == "ios":
        flags += ["--ios_multi_cpus=sim_arm64", "--apple_platform_type=ios"]
    commands.run(startup + ["build", label] + flags, timeout=1800)
    bazel_bin = Path(commands.run(startup + ["info", "bazel-bin"] + flags))
    suffix = ".ipa" if platform == "ios" else ".zip"
    return extract_app(bazel_bin / "apple/NativeHarness" / (target + suffix), output / "artifact", platform)


def wait_report(path: Path, run_id: str, scenario: str, timeout: float, evidence: Path) -> dict:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.is_file():
            report = json.loads(path.read_text())
            atomic_json(evidence, report)
            validate_report(report, run_id, scenario)
            return report
        time.sleep(0.2)
    raise TimeoutError("native app did not produce a final report before deadline")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=["ios", "mac"], required=True)
    parser.add_argument("--scenario", choices=[*CHECKS, "all"], default="all")
    parser.add_argument("--run", action="store_true", help="explicitly allow build/install/launch")
    parser.add_argument("--output", type=Path, help="absolute, new evidence directory")
    cache_group = parser.add_mutually_exclusive_group()
    cache_group.add_argument("--build-cache", type=Path,
        help="dedicated persistent cache (never deleted); default ~/Library/Caches/Alondra/native-harness")
    cache_group.add_argument("--fresh-build", action="store_true", help="explicitly rebuild with a disposable cache")
    parser.add_argument("--runtime", help="installed iOS simulator runtime identifier; required for iOS --run")
    parser.add_argument("--device-type", default="com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro")
    parser.add_argument("--timeout", type=float, default=90)
    args = parser.parse_args(argv)
    scenarios = list(CHECKS) if args.scenario == "all" else [args.scenario]
    if not 1 <= args.timeout <= 600:
        parser.error("timeout must be between 1 and 600 seconds")
    plan = {"mode": "plan", "platform": args.platform, "scenarios": scenarios,
            "scope": "native-client-with-synthetic-http-peers", "realBackend": False,
            "uiNavigation": False, "keychain": False, "buildConfiguration": "dbg", "simulator": "new owned device" if args.platform == "ios" else None}
    if not args.run:
        print(json.dumps(plan, indent=2))
        return 0
    if args.output is None or not args.output.is_absolute() or args.output.exists():
        parser.error("--run needs --output pointing to a new absolute directory")
    if args.platform == "ios" and not args.runtime:
        parser.error("iOS --run needs an explicit installed --runtime")
    if sys.platform != "darwin":
        parser.error("native harness requires macOS")
    dirty = subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True)
    if dirty:
        parser.error("commit harness source before executing; clean checkout required")
    cache = (args.output / "bazel") if args.fresh_build else (args.build_cache or Path.home() / "Library/Caches/Alondra/native-harness")
    if not cache.is_absolute():
        parser.error("--build-cache must be absolute")
    if min(free_bytes(args.output), free_bytes(cache)) < 8 * 1024**3:
        parser.error("at least 8 GiB free required on both output and cache volumes")
    os.umask(0o077)
    args.output.mkdir(mode=0o700)
    output = args.output.resolve()
    commands = Commands(output)
    run_id = str(uuid.uuid4())
    manifest = {**plan, "mode": "running", "runID": run_id, "dirty": False,
                "reports": [], "cleanup": "pending"}
    atomic_json(output / "manifest.json", manifest)
    owned_udid = None
    cache_lock = None
    old_signals = {value: signal.getsignal(value) for value in (signal.SIGINT, signal.SIGTERM)}
    for value in old_signals:
        signal.signal(value, interrupted)
    try:
        manifest["source"] = commands.run(["git", "rev-parse", "HEAD"])
        manifest["platformVersion"] = commands.run(["sw_vers", "-productVersion"])
        manifest["xcode"] = commands.run(["xcodebuild", "-version"])
        cache_lock = acquire_cache(cache)
        manifest["buildCache"] = str(cache)
        manifest["freshBuild"] = args.fresh_build
        atomic_json(output / "manifest.json", manifest)
        app = build_app(commands, output, cache, args.platform)
        executable = app_executable(app, args.platform)
        manifest["binarySHA256"] = hash_file(executable)
        manifest["appPath"] = str(app)
        if args.platform == "ios":
            # Never boot, erase, install into or delete a user's existing simulator.
            owned_udid = commands.run(["xcrun", "simctl", "create", "Alondra-native-" + run_id,
                                       args.device_type, args.runtime])
            uuid.UUID(owned_udid)
            manifest["ownedSimulator"] = owned_udid
            manifest["runtime"] = args.runtime
            atomic_json(output / "manifest.json", manifest)
            commands.run(["xcrun", "simctl", "boot", owned_udid])
            commands.run(["xcrun", "simctl", "bootstatus", owned_udid, "-b"], timeout=180)
            commands.run(["xcrun", "simctl", "install", owned_udid, str(app)])
            container = Path(commands.run(["xcrun", "simctl", "get_app_container", owned_udid, BUNDLE, "data"]))
            result_path = container / "Documents/native-result.json"
        for scenario in scenarios:
            scenario_run = str(uuid.uuid4())
            if args.platform == "mac":
                result_path = output / (scenario + ".json")
            elif result_path.exists():
                result_path.unlink()
            env = base_environment()
            values = {"ALONDRA_TEST_RUN_ID": scenario_run, "ALONDRA_TEST_SCENARIO": scenario}
            if args.platform == "ios":
                env.update({"SIMCTL_CHILD_" + key: value for key, value in values.items()})
                commands.run(["xcrun", "simctl", "launch", "--terminate-running-process", owned_udid, BUNDLE], env=env)
            else:
                env.update(values)
                env["ALONDRA_TEST_OUTPUT"] = str(result_path)
                commands.run([str(executable)], env=env, timeout=args.timeout)
            report = wait_report(result_path, scenario_run, scenario, args.timeout, output / (scenario + ".json"))
            atomic_json(output / (scenario + ".json"), report)
            manifest["reports"].append({"scenario": scenario, "runID": scenario_run, "status": "passed",
                                        "sha256": hash_file(output / (scenario + ".json"))})
            atomic_json(output / "manifest.json", manifest)
        manifest["mode"] = "passed"
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        manifest["mode"] = "failed"
        manifest["error"] = str(error)
    finally:
        # A second Ctrl-C/SIGTERM must not interrupt ownership-based cleanup.
        for value in old_signals:
            signal.signal(value, signal.SIG_IGN)
        try:
            errors = cleanup_owned(commands, owned_udid, cache, cache_lock, args.fresh_build)
            manifest["cleanup"] = "failed" if errors else "complete"
            if errors:
                manifest["cleanupErrors"] = errors
                manifest["mode"] = "failed"
            atomic_json(output / "manifest.json", manifest)
        finally:
            for value, previous in old_signals.items():
                signal.signal(value, previous)
    print(json.dumps({"status": manifest["mode"], "manifest": str(output / "manifest.json")}, indent=2))
    return 0 if manifest["mode"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
