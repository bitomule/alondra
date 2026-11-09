#!/usr/bin/env python3
"""Record a complete iOS baseline set for an OS migration, without copying stale outputs."""
import os
from pathlib import Path
import subprocess
import uuid

from simulator import config, probe


TARGETS = {
    "apple/Testing/Snapshots/AlondraSnapshotMigrationTests": "apple/Testing/Snapshots/Tests/__Snapshots__",
    "apple/Mermaid/AlondraMermaidSnapshotTests": "apple/Mermaid/Snapshots/__Snapshots__",
}


def collect(root, logs, generation):
    expected = {path.relative_to(root) for directory in set(TARGETS.values())
                for path in (root / directory).rglob("*.png")}
    if not expected:
        raise ValueError("No existing image references to migrate")
    captured = {}
    for target, directory in sorted(TARGETS.items()):
        outputs = logs / target / "test.outputs" / generation
        for path in sorted((outputs / directory).rglob("*.png")):
            relative = path.relative_to(outputs)
            if relative in captured:
                raise ValueError(f"Duplicate reference in recording: {relative}; no files copied")
            captured[relative] = path
    if set(captured) != expected:
        raise ValueError(f"Incomplete recording: {len(expected - set(captured))} missing and "
                         f"{len(set(captured) - expected)} unexpected references; no files copied")
    return captured


def copy_references(root, captured):
    for relative, source in sorted(captured.items()):
        (root / relative).write_bytes(source.read_bytes())


def main():
    if os.environ.get("GITHUB_ACTIONS") == "true":
        raise RuntimeError("References are recorded locally, never in GitHub Actions")
    root = Path(__file__).resolve().parents[1]
    probe(root / ".scratch/simulator-probe")
    generation = "record-" + uuid.uuid4().hex
    result = subprocess.run([
        "bazelisk", "test", *("//" + target.rsplit("/", 1)[0] + ":" + target.rsplit("/", 1)[1] for target in TARGETS),
        "--xcode_version=" + config()["xcode_version"],
        "--test_env=SNAPSHOT_TESTING_RECORD=all", "--test_env=SNAPSHOT_RECORD_GENERATION=" + generation,
        "--nocache_test_results", "--zip_undeclared_test_outputs=false", "--test_output=errors",
    ], cwd=root)
    # Recording a reference makes swift-snapshot-testing fail its comparison. Bazel's 3 is
    # TESTS_FAILED; build failures and interrupted runs must never copy test outputs.
    if result.returncode not in (0, 3):
        return result.returncode
    captured = collect(root, root / "bazel-testlogs", generation)
    copy_references(root, captured)
    print(f"Recorded {len(captured)} references on the canonical simulator. Run verification before committing.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
