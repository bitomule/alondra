#!/usr/bin/env python3
"""Deletes the inside of a component, has a model write it again from its specs, and measures it.

    make regeneration-pilot COMPONENT=spec-engine RUNS=5 MODEL=sonnet
    make regeneration-pilot COMPONENT=spec-engine RUNS=1 MODEL=sonnet BASELINE=1

or, with any other command that writes code:

    python3 tools/regeneration_pilot.py spec-engine --runs 5 \\
        --packet "$(alondra-spec packet spec-engine)" --generator '<command> {prompt}'

Each run gets a temporary folder of its own, outside this checkout so that the generator cannot
walk up to the code it replaces, with only what an implementer may see: the packet of the
component, its tests without the ones of reserved scenarios, its fixtures, the snapshot of its
public surface and a manifest to build it alone. The generator command runs there and writes the
files the boundary says may be regenerated. The result is then copied over the component in this
checkout, measured with the component's real suites, and the original files are put back. What
each run wrote and said is kept under `.scratch/pilot/`.

A run with `--baseline` hands the generator a short description and the public surface instead of
the packet and the tests: what the same model writes without the specs.

With `--jobs`, that many generators write at the same time, each in its folder; what they wrote is
always measured one run after another, because measuring replaces files of this checkout.

It costs tokens and builds, so it is run by hand on a clean checkout, never by a test. The same
command gives another result each time: it is repeatable, not reproducible to the byte.
"""

import argparse
import json
import re
import shlex
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import tomllib
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WORK = ROOT / ".scratch" / "pilot"
TEST_LINE = re.compile(r"^test (\S+) \.\.\. (ok|FAILED|ignored)", re.M)
AGENT_SESSION = ("ALONDRA_", "CLAUDE")
MARKER = re.compile(r"^\s*(?://+|#)\s*spec:\s*(\S+)\s*$", re.M)

PROMPT = """You are implementing the inside of one component of a larger repository, from its specs.

Read PACKET.md first: it holds the specs, the boundary and the decisions of the component. The
specs are the only source of truth.

Write the files the packet says you may write, and nothing else: {regenerates}
The tests under the component's `tests` folder verify the scenarios; `{surface}` lists the public
surface your code must have, item for item. You may build and run the tests in this folder with
`cargo test --offline`. Work until every test passes. Do not change the tests, the fixtures, the
packet or the manifest.
"""

BASELINE_PROMPT = """Write a Rust library, in {regenerates}, for this purpose:

{description}

`{surface}` lists the public surface it must have, item for item. You may build it in this folder
with `cargo build --offline`.
"""

DESCRIPTION = """A spec engine. It reads, through a `Files` trait its caller implements, a project's
`alondra.toml` settings, its specs (feature files in Gherkin syntax with tags that carry
identifiers), the `boundary.toml` of each component, decision records and reference prose; finds
the comments in test files that declare which scenario a test verifies; computes SHA-256
fingerprints of what each validation depends on; reads a ledger of recorded validations; and
reports which validations are valid, missing or stale. It never touches the disk itself."""


def run(command, cwd, **options):
    return subprocess.run(command, cwd=cwd, text=True, capture_output=True, **options)


def generate(command, cwd, minutes):
    """Runs the generator in a process group of its own and stops all of it at the time limit.

    When an agent session launches the pilot, the generator does not inherit what identifies that
    session: it is a session of its own.
    """
    own = {name: value for name, value in os.environ.items() if not name.startswith(AGENT_SESSION)}
    process = subprocess.Popen(command, cwd=cwd, env=own, shell=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True)
    try:
        said, _ = process.communicate(timeout=minutes * 60)
        return process.returncode == 0, said
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        said, _ = process.communicate()
        return False, said + f"\nstopped after {minutes} minutes\n"


def boundary_of(component):
    return tomllib.loads((ROOT / "specs" / component / "boundary.toml").read_text())


def matches(pattern, path):
    parts = [re.escape(part).replace(r"\*", "[^/]*") for part in pattern.split("/")]
    expression = "/".join(parts).replace("[^/]*[^/]*/", "(?:.*/)?").replace("/[^/]*[^/]*", "(?:/.*)?")
    return re.fullmatch(expression, path) is not None


def tracked(prefix):
    listed = run(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", prefix], ROOT).stdout
    return [path for path in listed.split("\0") if path and (ROOT / path).is_file()]


def reserved_scenarios(component):
    """`<spec>/<scenario>` of every scenario tagged `@reserved` in the component's feature files."""
    found = set()
    for feature in sorted((ROOT / "specs" / component).glob("*.feature")):
        spec = None
        for line in feature.read_text().splitlines():
            tags = line.split()
            if not tags or not all(tag.startswith("@") for tag in tags):
                continue
            named = {tag.split(":", 1)[0]: tag.split(":", 1)[-1] for tag in tags}
            spec = named.get("@spec", spec)
            if "@reserved" in tags and "@id" in named:
                found.add(f"{spec}/{named['@id']}")
    return found


def withheld_tests(boundary, reserved):
    """The test files that only verify reserved scenarios. A file that mixes them is a mistake."""
    withheld = []
    for suite in boundary.get("suite", []):
        for pattern in suite.get("tests", []):
            for path in tracked(pattern.split("*")[0]):
                if not matches(pattern, path):
                    continue
                declared = set(MARKER.findall((ROOT / path).read_text()))
                if declared and declared <= reserved:
                    withheld.append(path)
                elif declared & reserved:
                    sys.exit(f"{path} mixes tests of reserved scenarios with others: give the reserved ones a file of their own")
    return withheld


def standalone_manifest(component_dir):
    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]
    manifest = tomllib.loads((ROOT / component_dir / "Cargo.toml").read_text())
    lines = ["[package]", f'name = "{manifest["package"]["name"]}"', 'version = "0.0.0"', f'edition = "{workspace["package"]["edition"]}"', "autotests = false", "", "[workspace]", ""]
    for table in ("dependencies", "dev-dependencies"):
        lines.append(f"[{table}]")
        for name in manifest.get(table, {}):
            pinned = workspace["dependencies"][name]
            if isinstance(pinned, str):
                lines.append(f'{name} = "{pinned}"')
            else:
                features = ", ".join(f'"{feature}"' for feature in pinned.get("features", []))
                defaults = "" if pinned.get("default-features", True) else ", default-features = false"
                lines.append(f'{name} = {{ version = "{pinned["version"]}", features = [{features}]{defaults} }}')
        lines.append("")
    lines += ["[[test]]", 'name = "all"', 'path = "tests/all.rs"', ""]
    return "\n".join(lines)


def prepare(folder, component, boundary, packet, withheld, baseline):
    """The folder a generator works in: the component without what it must write or must not see."""
    regenerates = boundary.get("regenerates", [])
    owned_root = boundary["owns"][0].split("*")[0].rstrip("/")
    target = folder / owned_root
    for path in tracked(owned_root):
        if any(matches(pattern, path) for pattern in regenerates) or path in withheld:
            continue
        if baseline and "/tests/" in path:
            continue
        (folder / path).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / path, folder / path)
    modules = {Path(path).stem for path in withheld}
    root_test = target / "tests" / "all.rs"
    if root_test.exists():
        kept = [line for line in root_test.read_text().splitlines() if not (line.startswith("mod ") and line[4:].rstrip(";") in modules)]
        root_test.write_text("\n".join(kept) + "\n")
    (target / "Cargo.toml").write_text(standalone_manifest(owned_root))
    shutil.copyfile(ROOT / "Cargo.lock", target / "Cargo.lock")
    surface = boundary.get("surface")
    if surface:
        shutil.copyfile(ROOT / surface, target / "SURFACE.txt")
    names = {"regenerates": ", ".join(pattern.removeprefix(owned_root + "/") for pattern in regenerates), "surface": "SURFACE.txt"}
    if baseline:
        prompt = BASELINE_PROMPT.format(description=DESCRIPTION, **names)
    else:
        (target / "PACKET.md").write_text(packet)
        prompt = PROMPT.format(**names)
    (target / "PROMPT.md").write_text(prompt)
    return target, owned_root


def swap(owned_root, regenerates, source):
    """Replaces the regenerated files of this checkout with the ones under `source`."""
    for path in tracked(owned_root):
        if any(matches(pattern, path) for pattern in regenerates):
            (ROOT / path).unlink()
    for path in sorted(source.rglob("*")):
        relative = f"{owned_root}/{path.relative_to(source)}"
        if path.is_file() and any(matches(pattern, relative) for pattern in regenerates):
            (ROOT / relative).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, ROOT / relative)


def measure(boundary, withheld):
    """Runs the component's real suites on what is in this checkout now."""
    result = {"suites": {}, "tests": {"passed": 0, "failed": 0}, "reserved": {"passed": 0, "failed": 0}}
    reserved_modules = {Path(path).stem for path in withheld}
    for suite in boundary.get("suite", []):
        finished = run(suite["run"] + ["--test_output=all"], ROOT)
        result["suites"][suite["id"]] = finished.returncode == 0
        for name, outcome in TEST_LINE.findall(finished.stdout):
            group = "reserved" if name.split("::")[0] in reserved_modules else "tests"
            if outcome != "ignored":
                result[group]["passed" if outcome == "ok" else "failed"] += 1
    result["works"] = all(result["suites"].values()) and bool(result["suites"])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("component")
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--packet", help="the text `alondra spec packet <component>` prints")
    parser.add_argument("--generator", required=True, help="the command that writes the code; {prompt} is the path of its instructions; it runs in the folder of the run")
    parser.add_argument("--baseline", action="store_true", help="hand over a short description instead of the specs and the tests")
    parser.add_argument("--label", default=None, help="the name of this arm in the results")
    parser.add_argument("--timeout-minutes", type=int, default=60)
    parser.add_argument("--jobs", type=int, default=1, help="how many generators write at the same time; results are always measured one after another")
    arguments = parser.parse_args()
    if not arguments.baseline and not arguments.packet:
        parser.error("--packet is required unless --baseline is given")
    if run(["git", "status", "--porcelain"], ROOT).stdout.strip():
        sys.exit("the checkout has changes: commit them first, the pilot replaces files of the component")

    boundary = boundary_of(arguments.component)
    regenerates = boundary.get("regenerates", [])
    withheld = withheld_tests(boundary, reserved_scenarios(arguments.component))
    label = arguments.label or ("baseline" if arguments.baseline else "specs")
    def generated(number):
        kept = WORK / label / f"run-{number}"
        shutil.rmtree(kept, ignore_errors=True)
        kept.mkdir(parents=True)
        folder = Path(tempfile.mkdtemp(prefix="regeneration-"))
        target, owned_root = prepare(folder, arguments.component, boundary, arguments.packet, withheld, arguments.baseline)
        command = arguments.generator.replace("{prompt}", shlex.quote(str(target / "PROMPT.md")))
        finished, said = generate(command, target, arguments.timeout_minutes)
        (kept / "generator.log").write_text(said)
        wrote = 0
        for path in sorted(target.rglob("*")):
            relative = path.relative_to(target)
            if path.is_file() and any(matches(pattern, f"{owned_root}/{relative}") for pattern in regenerates):
                (kept / relative).parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(path, kept / relative)
                wrote += 1
        return number, folder, target, owned_root, finished, wrote

    results = []
    with ThreadPoolExecutor(max_workers=arguments.jobs) as generators:
        for number, folder, target, owned_root, finished, wrote in generators.map(generated, range(1, arguments.runs + 1)):
            try:
                if wrote:
                    swap(owned_root, regenerates, target)
                    result = measure(boundary, withheld)
                else:
                    result = {"works": False}
            finally:
                run(["git", "checkout", "--", owned_root], ROOT)
                run(["git", "clean", "-fdq", "--", owned_root], ROOT)
                shutil.rmtree(folder, ignore_errors=True)
            result.update({"run": number, "arm": label, "generator_finished": finished, "files_written": wrote})
            results.append(result)
            print(json.dumps(result), flush=True)
    written = [result for result in results if result["files_written"]]
    summary = {
        "component": arguments.component,
        "arm": label,
        "runs": len(results),
        "written": len(written),
        "works": sum(result["works"] for result in written),
        "results": results,
    }
    (WORK / f"{label}.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(f"{summary['works']} of {summary['written']} written runs pass every suite ({label}); details in {WORK / (label + '.json')}")
    if len(written) < len(results):
        sys.exit(f"{len(results) - len(written)} of {len(results)} generators wrote nothing: see their generator.log under {WORK / label}")


if __name__ == "__main__":
    main()
