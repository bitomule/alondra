#!/usr/bin/env python3
"""Run affected MUSTS checks for external PRs or a manual runner and timing probe."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import time

from musts_scopes import matches, parse_checks


MAC_CHECKS = {
    "crate-agent-runtime", "crate-alondra", "crate-alondrad", "crate-core", "crate-daemon-engine",
    "crate-daemon-support", "crate-process-group", "crate-provider-claude",
    "crate-provider-codex", "local-daemon", "build-from-source",
    "sourcekit-lsp-workspace", "deploy-brew", "deploy-apple", "apple-daemon-process",
    "apple-tests", "sdk-host", "reap-test-hosts", "native-fixture-lifecycle",
}
LINUX_CHECKS = {
    "crate-agent-application", "crate-agent-protocol", "crate-codex-protocol",
    "crate-domain", "crate-e2e", "crate-foundation", "crate-provider-contract", "crate-relay",
    "crate-store", "crates-root", "licenses", "prose-decisions", "bazel-artifact-paths",
    "sandbox-accounts", "low-opt-rust-tests", "sandbox-tool", "apple-scripts", "apple-catalogs",
    "plugin-mod", "cargo-bazel-deps", "load-bench-oracles", "buildbuddy-credential-helper",
    "public-build-inputs", "bazel-test-tmpdir", "musts-scopes", "test-durations", "test-limits",
    "worktree-setup", "ci", "simulator", "snapshot-recording",
}
BOOTSTRAP = {
    "MUSTS.yml", "tools/ci.py", "tools/musts_scopes.py", ".github/workflows/ci.yml",
    ".github/ci-tools/BUILD.bazel", ".github/ci-tools/package.json", ".github/ci-tools/package-lock.json",
}
TARGET = re.compile(r"//[A-Za-z0-9_./+-]*:[A-Za-z0-9_./+-]+$|//[A-Za-z0-9_./+-]*/\.\.\.$")
# Bazel's documented exit status: the build succeeded but the target set contains no tests.
NO_TESTS_FOUND = 4
MANUAL_CHECKS = {"mac-inventory-review"}


def manual_reviews(text):
    reviews = {}
    blocks = re.finditer(r"^  ([\w-]+):\n(.*?)(?=^  [\w-]+:|\Z)", text, re.M | re.S)
    for block in blocks:
        name, body = block.groups()
        if "    uses: agent\n" not in body:
            continue
        paths = re.findall(r'^      - (".*")$', body, re.M)
        facts = re.findall(r'^        - (".*")$', body, re.M)
        if name not in MANUAL_CHECKS or not paths or not facts or "      targets:" in body:
            raise ValueError(f"Unsupported manual review check: {name}")
        reviews[name] = {"paths": [json.loads(p) for p in paths], "facts": [json.loads(f) for f in facts]}
    return reviews


def manifest(text):
    """Accept the manifest's current block syntax; refuse extensions instead of losing checks."""
    grammar = re.compile(
        r'version: 1|checks:|  [\w-]+:|    uses: (?:bazel/test|agent)|    paths:|    with:|'
        r'      targets:|      facts:|      - "[^"\\#\n]+"|        - "[^"\\\n]+"|        - //[^\s#]+'
    )
    lines = []
    for line in text.splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if not grammar.fullmatch(line):
            raise ValueError(f"Unsupported MUSTS syntax: {line!r}; extend the CI parser with tests")
        lines.append(line)
    if lines[:2] != ["version: 1", "checks:"]:
        raise ValueError("Expected version: 1 and checks:")
    names = [line[2:-1] for line in lines if re.fullmatch(r"  [\w-]+:", line)]
    if len(names) != len(set(names)):
        raise ValueError("Duplicate MUSTS check")
    checks = parse_checks(text)
    reviews = manual_reviews(text)
    if not checks or set(checks) | set(reviews) != set(names):
        raise ValueError("Every MUSTS check must be a Bazel check or an explicit manual review")
    if unknown := set(checks) - MAC_CHECKS - LINUX_CHECKS:
        raise ValueError(f"Assign a CI runner to new MUSTS checks: {sorted(unknown)}")
    for name, spec in checks.items():
        if not spec["paths"] or not spec["targets"] or any(not TARGET.fullmatch(t) for t in spec["targets"]):
            raise ValueError(f"Missing paths or invalid targets in {name}")
    return checks


def affected_reviews(current, previous, paths, force_all=False):
    selected = {}
    for reviews in (previous, current):
        for name, spec in reviews.items():
            if force_all or paths & BOOTSTRAP or any(matches(spec["paths"], path) for path in paths):
                selected.setdefault(name, set()).update(spec["facts"])
    return {name: sorted(facts) for name, facts in sorted(selected.items())}


def external_pr(event):
    pr = event["pull_request"]
    owner = event["repository"]["owner"]["login"]
    return pr["user"]["login"] != owner and (
        pr["author_association"] != "OWNER" or pr["head"]["repo"]["fork"]
    )


def changed_paths(raw):
    fields = iter(raw.split(b"\0"))
    paths = set()
    for status in fields:
        if not status:
            continue
        count = 2 if status[:1] in (b"R", b"C") else 1
        for _ in range(count):
            path = next(fields, b"")
            if not path:
                raise ValueError("Truncated git name-status output")
            paths.add(os.fsdecode(path))
    return paths


def plan(current, previous, paths, force_all=False):
    selected = {}
    all_checks = force_all or bool(paths & BOOTSTRAP)
    for checks in (previous, current):
        for name, spec in checks.items():
            if all_checks or any(matches(spec["paths"], path) for path in paths):
                selected.setdefault(name, set()).update(spec["targets"])
    jobs = []
    for platform, runner in (("linux", "ubuntu-24.04"), ("macos", "xcode-27")):
        names = [name for name in sorted(selected) if (name in MAC_CHECKS) == (platform == "macos")]
        # A separate target set per check bounds each fresh runner's disk use. Targets shared by
        # checks on the same OS run once, including checks whose patterns are identical.
        seen = set()
        for name in names:
            targets = sorted(selected[name] - seen)
            seen.update(targets)
            if targets:
                jobs.append({"check": name, "runner": runner, "targets": targets,
                             "timeout": 120 if platform == "macos" else 90})
    jobs.sort(key=lambda job: job["check"] != "apple-tests")
    return {"include": jobs}


def command(*args):
    return subprocess.check_output(args)


def write_output(name, value):
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"{name}={value}\n")


def selection_base(event_name, event):
    if event_name == "workflow_dispatch":
        branch = event["repository"]["default_branch"]
        return command("git", "rev-parse", "--verify", f"refs/remotes/origin/{branch}^{{commit}}").decode().strip()
    if event_name == "pull_request" and external_pr(event):
        return event["pull_request"]["base"]["sha"]
    raise ValueError("Expected an external pull request or an explicit manual dispatch")


def manual_scope(event_name, event, current, previous):
    if event_name != "workflow_dispatch":
        return current, previous, ""
    requested = event.get("inputs", {}).get("check", "")
    if not isinstance(requested, str):
        raise ValueError("The manual check must be a MUSTS check name")
    requested = requested.strip()
    if requested:
        if requested not in current and requested not in previous:
            raise ValueError(f"Unknown MUSTS check: {requested}")
        current = {name: spec for name, spec in current.items() if name == requested}
        previous = {name: spec for name, spec in previous.items() if name == requested}
    return current, previous, requested


def select(args):
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    event_name = os.environ["GITHUB_EVENT_NAME"]
    base = selection_base(event_name, event)
    if not re.fullmatch(r"[a-f0-9]{40}", base):
        raise ValueError("Invalid base SHA")
    ancestor = command("git", "merge-base", base, "HEAD").decode().strip()
    paths = changed_paths(command("git", "diff", "--name-status", "-z", "--find-renames", f"{ancestor}...HEAD", "--"))
    current_text = Path("MUSTS.yml").read_text()
    previous_text = command("git", "show", f"{ancestor}:MUSTS.yml").decode()
    current = manifest(current_text)
    previous = manifest(previous_text)
    current, previous, requested = manual_scope(event_name, event, current, previous)
    probe = event_name == "workflow_dispatch"
    matrix = plan(current, previous, paths, force_all=probe)
    reviews = affected_reviews(manual_reviews(current_text), manual_reviews(previous_text), paths,
                               force_all=probe and not requested)
    write_output("matrix", json.dumps(matrix, separators=(",", ":")))
    write_output("has_checks", str(bool(matrix["include"])).lower())
    print(json.dumps({"event": event_name, "check": requested or None,
                      "base": ancestor, "paths": sorted(paths), "matrix": matrix,
                      "manual_reviews": reviews}, indent=2))
    with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as summary:
        heading = "Runner and timing probe" if probe else "External contribution validation"
        summary.write(f"## {heading}\n\n{len(matrix['include'])} target sets selected from MUSTS.yml.\n")
        if probe:
            summary.write("This runner probe does not validate the external-contributor or fork-approval path.\n")
            summary.write(f"\nRequested scope: **{requested or 'all checks'}**.\n")
        if not matrix["include"]:
            summary.write("No deterministic Bazel check reads the changed paths.\n")
        for name, facts in reviews.items():
            summary.write(f"\n### Manual review required: {name}\n\nCI does not perform or approve this judgment check.\n")
            for fact in facts:
                summary.write(f"\n- {fact}\n")


def run(args):
    targets = json.loads(args.targets)
    if not isinstance(targets, list) or not targets or any(not isinstance(t, str) or not TARGET.fullmatch(t) for t in targets):
        raise ValueError("Expected a nonempty JSON list of Bazel target labels")
    start = time.monotonic()
    startup = ["bazelisk", "--nosystem_rc", "--nohome_rc"]
    mode = "test"
    result = subprocess.run([*startup, mode, "--config=ci", "--test_output=errors", "--", *targets])
    if result.returncode == NO_TESTS_FOUND:
        mode = "build"
        result = subprocess.run([*startup, mode, "--config=ci", "--", *targets])
    elapsed = time.monotonic() - start
    report = {"targets": targets, "seconds": round(elapsed, 3), "exit_code": result.returncode, "mode": mode}
    print(json.dumps(report))
    if summary_path := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary_path, "a") as summary:
            summary.write(f"\nBazel build and tests: **{elapsed:.1f} s**, exit **{result.returncode}**.\n")
            summary.write("\nTargets: " + ", ".join(f"`{t}`" for t in targets) + "\n")
    return result.returncode


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(required=True)
    sub.add_parser("plan").set_defaults(action=select)
    runner = sub.add_parser("run")
    runner.add_argument("--targets", required=True)
    runner.set_defaults(action=run)
    args = parser.parse_args()
    return args.action(args)


if __name__ == "__main__":
    raise SystemExit(main())
