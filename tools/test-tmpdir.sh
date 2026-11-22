#!/bin/bash
# Wraps every `bazel test` (`test --run_under` in tools/test-wrapper.bazelrc, which a user bazelrc
# with its own --run_under imports last). The test's TMPDIR is the TEST_TMPDIR Bazel gives it, emptied
# when the test exits, fails or is cut, so tests that never clean their temp dirs cannot fill the disk.
# macOS caps a unix socket path at 104 bytes and TEST_TMPDIR is longer than that leaves room for, so
# the wrapper also makes a private short folder, ALONDRA_SHORT_TMPDIR, only for the fixtures that bind
# a socket (and for xcrun's lookup cache), removed with the test too. Whatever still runs from either
# folder when the test ends dies with its process group, and a passing test that left an Alondra host
# (a process whose stdin is a host.lock, the host's ownership lock) fails.
#
# The test may execute only itself and the interpreters of its script: every other program it runs is
# listed in its target's TEST_EXECS, which the guard below turns into a kernel sandbox that refuses
# any other exec (docs/running-tests.md, "Programs a test runs"). //tools:test_wrapper_test fails
# when a test runs under any other wrapper, through ALONDRA_TEST_WRAPPER.
root=${BAZEL_TEST_TMPDIR_ROOT:-/tmp}
export ALONDRA_TEST_WRAPPER=1
# Python's execvpe reports invalid formats instead of interpreting them as shell commands.
launcher='
import errno
import hashlib
import os
import sys


def describe(path):
    try:
        resolved = os.path.realpath(path)
        status = os.stat(resolved)
        with open(resolved, "rb") as file:
            head = file.read(16)
            digest = hashlib.sha256(head + file.read()).hexdigest()
        volume = "boot volume" if status.st_dev == os.stat("/").st_dev else "another volume"
        return (
            f"Evidence: {resolved} on the {volume}, {status.st_size} bytes, sha256 {digest}, "
            f"first bytes {head.hex()}, modified {int(status.st_mtime)}"
        )
    except OSError as problem:
        return f"Evidence unavailable: {problem}"


UNGUARDED = {"own-sandbox", "e2e"}
# Swift tests drive simulators and Xcode; the opt-in suites of tests/ run real daemons on request.
UNGUARDED_PACKAGES = ("//apple/", "//tests/")
# rustdoc compiles every example with the C toolchain and runs it from its own temporary folder.
DOCTEST = ".rustdoc_test.sh"
TEMPORARY = "tmpdir"
ON_PATH = "path:"
FRAMEWORK = ".framework/Versions/"
# What the launchers Bazel and this repository generate for script tests run before the
# interpreter: rules_js, rules_rust doctests and the Python test runner.
LAUNCHER_TOOLS = ("/usr/bin/uname", "/usr/bin/dirname", "/usr/bin/basename", "/bin/ln", "/usr/bin/env", "/bin/bash")


def developer_dir():
    if os.environ.get("DEVELOPER_DIR"):
        return os.environ["DEVELOPER_DIR"]
    try:
        return os.readlink("/var/db/xcode_select_link")
    except OSError:
        return None


def program(path, developer):
    """The paths the kernel checks when `path` runs: itself, what it resolves to, and the program an
    Xcode shim or /bin/sh runs in its place."""
    real = os.path.realpath(path)
    literals, subpaths = {path, real}, set()
    if FRAMEWORK in real:
        head, tail = real.split(FRAMEWORK, 1)
        subpaths.add(head + FRAMEWORK + tail.split("/", 1)[0])
    if path == "/bin/sh":
        literals.add(os.path.realpath("/private/var/select/sh"))
        literals.add("/bin/bash")
    if os.path.dirname(path) == "/usr/bin" and developer:
        name = os.path.basename(path)
        for candidate in (f"{developer}/usr/bin/{name}", f"{developer}/usr/bin/gnu{name}", f"{developer}/Toolchains/XcodeDefault.xctoolchain/usr/bin/{name}"):
            if os.path.exists(candidate):
                # A shim with no entry in xcrun_db for this HOME asks xcodebuild through /bin/sh.
                for lookup in ("/bin/sh", f"{developer}/usr/bin/xcodebuild"):
                    more_literals, more_subpaths = program(lookup, None)
                    literals |= more_literals
                    subpaths |= more_subpaths
                more_literals, more_subpaths = program(candidate, None)
                literals |= more_literals
                subpaths |= more_subpaths
        if os.path.isdir(f"{developer}/usr/libexec/{name}-core"):
            subpaths.add(os.path.realpath(f"{developer}/usr/libexec/{name}-core"))
    return literals, subpaths


def interpreter_chain(path, developer, depth=0):
    literals, subpaths = program(path, developer)
    try:
        with open(path, "rb") as file:
            first = file.readline(512)
    except OSError:
        first = b""
    if depth < 4 and first.startswith(b"#!"):
        words = first[2:].decode(errors="replace").split()
        interpreters = words[:1]
        if words and os.path.basename(words[0]) == "env":
            named = [word for word in words[1:] if not word.startswith("-")][:1]
            found = [shutil.which(word) for word in named]
            interpreters += [found[0]] if found and found[0] else []
        for interpreter in interpreters:
            more_literals, more_subpaths = interpreter_chain(interpreter, developer, depth + 1)
            literals |= more_literals
            subpaths |= more_subpaths
    return literals, subpaths


def toolchains(path):
    """Where Bazel keeps the programs of its external repositories, the toolchains a launcher runs
    (Node for a js_test, rustdoc for a doctest): never a program of this repository."""
    real = os.path.realpath(path)
    if "/execroot/" not in real or "/bazel-out/" not in real:
        return set(), set()
    output_base = real.split("/execroot/", 1)[0]
    execroot = real.split("/bazel-out/", 1)[0]
    escaped = "".join(c if c.isalnum() or c == "/" else chr(92) + c for c in execroot)
    return {output_base + "/external"}, {"^" + escaped + "/bazel-out/[^/]+/bin/external/"}


def runs(path, developer):
    """What running `path` executes: the program, the interpreters of its script and, for a script,
    what a Bazel launcher runs to start its interpreter."""
    literals, subpaths = interpreter_chain(path, developer)
    regexes = set()
    try:
        with open(path, "rb") as file:
            script = file.read(2) == b"#!"
    except OSError:
        script = False
    if script:
        for tool in LAUNCHER_TOOLS + tuple(filter(None, [shutil.which("python3")])):
            more_literals, more_subpaths = interpreter_chain(tool, developer)
            literals |= more_literals
            subpaths |= more_subpaths
        more_subpaths, regexes = toolchains(path)
        subpaths |= more_subpaths
    return literals, subpaths, regexes


def guard(test, declared):
    developer = developer_dir()
    literals, subpaths, regexes = runs(shutil.which(test) or test, developer)
    for entry in declared:
        if entry == TEMPORARY:
            for variable in ("TMPDIR", "TEST_TMPDIR"):
                if os.environ.get(variable):
                    subpaths.add(os.path.realpath(os.environ[variable]))
            continue
        if entry.startswith(ON_PATH):
            found = shutil.which(entry[len(ON_PATH):])
            if not found:
                continue
            entry = found
        more_literals, more_subpaths, more_regexes = runs(os.path.abspath(entry), developer)
        literals |= more_literals
        subpaths |= more_subpaths
        regexes |= more_regexes

    def quoted(path):
        return chr(34) + path.replace(chr(92), chr(92) * 2).replace(chr(34), chr(92) + chr(34)) + chr(34)

    def privileged(path):
        try:
            return os.stat(path).st_mode & 0o6000 != 0
        except OSError:
            return False

    # The kernel refuses a set-user-ID program (/bin/ps) to every sandboxed process; a declared one
    # runs outside the sandbox.
    setuid = sorted(path for path in literals if privileged(path))
    rules = [f"(literal {quoted(path)})" for path in sorted(literals) if path not in setuid]
    rules += [f"(subpath {quoted(path)})" for path in sorted(subpaths)]
    rules += [f"(regex #{chr(34)}{pattern}{chr(34)})" for pattern in sorted(regexes)]
    profile = "(version 1)(allow default)(deny process-exec*)(allow process-exec* " + " ".join(rules) + ")"
    if setuid:
        profile += "(allow process-exec* (with no-sandbox) " + " ".join(f"(literal {quoted(path)})" for path in setuid) + ")"
    library = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
    library.sandbox_init.argtypes = [ctypes.c_char_p, ctypes.c_uint64, ctypes.POINTER(ctypes.c_char_p)]
    library.sandbox_check.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int]
    problem = ctypes.c_char_p()
    if library.sandbox_init(profile.encode(), 0, ctypes.byref(problem)) != 0:
        if library.sandbox_check(os.getpid(), None, 0) == 1:
            return False
        print(f"exec guard: the sandbox could not start: {problem.value}", file=sys.stderr)
        sys.exit(125)
    return True


declared = os.environ.get("TEST_EXECS", "").split()
outside = os.environ.get("TEST_TARGET", "").startswith(UNGUARDED_PACKAGES) or sys.argv[1].endswith(DOCTEST)
if sys.platform == "darwin" and not outside and not UNGUARDED & set(declared):
    import ctypes
    import shutil
    if guard(sys.argv[1], declared) and os.environ.get("ALONDRA_TEST_GUARDED"):
        open(os.environ["ALONDRA_TEST_GUARDED"], "w").close()

try:
    os.execvpe(sys.argv[1], sys.argv[1:], os.environ)
except OSError as error:
    invalid_formats = {errno.ENOEXEC}
    invalid_formats.update(
        getattr(errno, name) for name in ("EBADEXEC", "EBADARCH", "EBADMACHO")
        if hasattr(errno, name)
    )
    if error.errno in invalid_formats:
        print(
            f"Invalid test executable: {sys.argv[1]} (errno {error.errno}). "
            "The operating system rejected its executable format; it may be corrupt.\n"
            "Rebuild the affected target in a fresh output base: "
            "bazelisk --output_base=<fresh-directory> test <target> "
            "--noremote_accept_cached --disk_cache= "
            "--noremote_upload_local_results --run_under=//tools:test_tmpdir",
            file=sys.stderr,
        )
        print(describe(sys.argv[1]), file=sys.stderr)
    else:
        print(f"Cannot execute test {sys.argv[1]}: {error}", file=sys.stderr)
    sys.exit(126 if error.errno != errno.ENOENT else 127)
'

remove() {
    chmod -R u+w "$1" 2>/dev/null
    rm -rf "$1"
}

empty() {
    chmod -R u+w "$1" 2>/dev/null
    find "$1" -mindepth 1 -maxdepth 1 -exec rm -rf {} + 2>/dev/null
}

for stale in "$root"/bt.*; do
    owner=$(cat "$stale/.owner" 2>/dev/null) || continue
    case $owner in '' | *[!0-9]*) continue ;; esac
    kill -0 "$owner" 2>/dev/null || remove "$stale"
done

dir=$(mktemp -d "$root/bt.XXXXXX") || exec python3 -c "$launcher" "$@"
echo $$ >"$dir/.owner"
export TMPDIR="$dir"
export ALONDRA_SHORT_TMPDIR="$dir"
scratch=
if [ -n "$TEST_TMPDIR" ] && [ -d "$TEST_TMPDIR" ]; then
    scratch=$(cd "$TEST_TMPDIR" && pwd -P)
    export TMPDIR="$scratch"
fi
# xcrun, and the /usr/bin shims of git, python3 and the other developer tools, add an entry to
# their lookup cache for every new HOME and rewrite the whole cache each time. Tests give every
# fixture its own HOME: in the user's temp folder that cache grew past 50 MB, and a tool killed
# while rewriting it left a full copy behind, gigabytes a day. Here it goes with the test.
export xcrun_db="$dir/xcrun_db"
export ALONDRA_TEST_GUARDED="$dir/.guarded"

left_running() {
    PATH=$PATH:/usr/sbin lsof -a -u "$(id -u)" -d cwd,0 -Fpn 2>/dev/null |
        awk -v inside="$(cd "$dir" && pwd -P)/" -v scratch="${scratch:+$scratch/}" '
            /^p/ { pid = substr($0, 2) }
            /^n/ && (index(substr($0, 2), inside) == 1 || (scratch != "" && index(substr($0, 2), scratch) == 1)) { print pid, ($0 ~ /\/host\.lock$/ ? "host" : "process") }'
}

end_left_running() {
    hosts=0
    left=$(left_running)
    for pid in $(echo "$left" | awk '{ print $1 }' | sort -u); do
        if echo "$left" | grep -q "^$pid host$"; then
            hosts=$((hosts + 1))
            echo "The test left an Alondra host running: $(ps -o pid=,command= -p "$pid")" >&2
        fi
        if [ "$(ps -o pgid= -p "$pid" | tr -d ' ')" = "$pid" ]; then
            kill -KILL -- "-$pid" 2>/dev/null
        else
            kill -KILL "$pid" 2>/dev/null
        fi
    done
    [ "$hosts" = 0 ]
}

child=
finish() {
    status=$1
    trap - EXIT INT TERM HUP
    if [ -n "$child" ]; then
        [ "$status" -gt 128 ] && kill -"$((status - 128))" "$child" 2>/dev/null
        wait "$child" 2>/dev/null
    fi
    end_left_running || [ "$status" != 0 ] || status=1
    [ ! -e "$dir/.guarded" ] || [ "$status" = 0 ] || echo "exec guard: this test may run only its own executable, the interpreter of its script and the programs its target lists in TEST_EXECS (docs/running-tests.md, \"Programs a test runs\"). Any other program fails with \"Operation not permitted\"; to see which, run the test again while \`log stream --style compact --predicate 'sender == \"Sandbox\" AND eventMessage CONTAINS \"process-exec\"'\` runs." >&2
    remove "$dir"
    [ -z "$scratch" ] || empty "$scratch"
    exit "$status"
}
trap 'finish $?' EXIT
trap 'finish 129' HUP
trap 'finish 130' INT
trap 'finish 143' TERM

python3 -c "$launcher" "$@" <&0 &
child=$!
wait "$child"
status=$?
child=
finish "$status"
