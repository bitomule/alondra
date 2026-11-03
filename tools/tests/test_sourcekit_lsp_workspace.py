#!/usr/bin/env python3
"""SourceKit-LSP opened on the checkout's root files starts the Bazel build server the repository
names, never a SwiftPM build of the root Package.swift into .build. A fake build server stands in
for sourcekit-bazel-bsp so the test needs no Bazel."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

ROOT_FILES = ["Package.swift", "Package.resolved", ".bsp/skbsp.json", ".sourcekit-lsp/config.json"]

FAKE_SERVER = """#!/usr/bin/env python3
import json, sys
from pathlib import Path
Path(sys.argv[0]).with_name("started").write_text(" ".join(sys.argv[1:]))
results = {
    "build/initialize": {"displayName": "fake", "version": "1", "bspVersion": "2.2.0",
        "capabilities": {"languageIds": ["swift"]}, "dataKind": "sourceKit",
        "data": {"sourceKitOptionsProvider": True}},
    "workspace/buildTargets": {"targets": []},
    "buildTarget/sources": {"items": []},
}
def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line.strip() == b"":
            break
        name, _, value = line.decode().partition(":")
        if name.lower() == "content-length":
            length = int(value)
    return json.loads(sys.stdin.buffer.read(length))
while (message := read()) is not None:
    if message.get("method") == "build/exit":
        break
    if "id" in message:
        body = json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": results.get(message["method"])}).encode()
        sys.stdout.buffer.write(b"Content-Length: %d\\r\\n\\r\\n" % len(body) + body)
        sys.stdout.buffer.flush()
"""


def runfile(relative):
    path = Path(os.environ["TEST_SRCDIR"]) / os.environ["TEST_WORKSPACE"] / relative
    return path if path.exists() else None


class SourceKitLspWorkspaceTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(dir=os.environ.get("TEST_TMPDIR"))
        self.addCleanup(directory.cleanup)
        self.home = Path(directory.name) / "home"
        self.home.mkdir()
        self.workspace = Path(directory.name) / "workspace"
        for relative in ROOT_FILES:
            source = runfile(relative)
            if source:
                (self.workspace / relative).parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, self.workspace / relative)
        connection = self.workspace / ".bsp/skbsp.json"
        if connection.exists():
            server = self.workspace / json.loads(connection.read_text())["argv"][0]
            server.parent.mkdir(parents=True, exist_ok=True)
            server.write_text(FAKE_SERVER)
            server.chmod(0o755)
            self.started = server.with_name("started")
        else:
            self.started = self.workspace / "no-build-server"
        self.source = self.workspace / "apple/A.swift"
        self.source.parent.mkdir(parents=True)
        self.source.write_text('let value: Int = "a"\n')

    def send(self, message):
        body = json.dumps({"jsonrpc": "2.0", **message}).encode()
        self.server.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        self.server.stdin.flush()

    def test_the_root_files_start_the_bazel_build_server_and_no_swiftpm_build(self):
        self.server = subprocess.Popen(
            ["xcrun", "sourcekit-lsp", "--bypass-workspace-trust"], cwd=self.workspace,
            stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            env={"PATH": "/usr/bin:/bin", "HOME": str(self.home)})
        self.addCleanup(self.server.wait)
        self.addCleanup(self.server.kill)
        root = self.workspace.as_uri()
        self.send({"id": 1, "method": "initialize", "params": {"processId": os.getpid(), "rootUri": root,
            "capabilities": {}, "workspaceFolders": [{"uri": root, "name": "workspace"}]}})
        self.send({"method": "initialized", "params": {}})
        self.send({"method": "textDocument/didOpen", "params": {"textDocument": {
            "uri": self.source.as_uri(), "languageId": "swift", "version": 1, "text": self.source.read_text()}}})

        deadline = time.monotonic() + 60
        while not self.started.exists() and not (self.workspace / ".build").exists() and time.monotonic() < deadline:
            time.sleep(0.2)

        self.assertFalse((self.workspace / ".build").exists(), "SourceKit-LSP built the root Package.swift with SwiftPM")
        self.assertTrue(self.started.exists(), "SourceKit-LSP never started the repository's build server")
        self.assertIn("--bazel-wrapper", self.started.read_text())


if __name__ == "__main__":
    unittest.main()
