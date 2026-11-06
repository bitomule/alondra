#!/usr/bin/env python3
"""Make's SDK environment resolves Bazel artifacts without an execroot external link."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(os.environ.get("TEST_SRCDIR", Path(__file__).resolve().parents[2]))
if "TEST_SRCDIR" in os.environ:
    ROOT /= os.environ.get("TEST_WORKSPACE", "_main")


class ArtifactPathsTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="artifact paths ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.base = self.root / "output base"
        self.execroot = self.base / "execroot/main"
        self.execroot.mkdir(parents=True)
        self.node_path = "external/pinned_node/bin/node"
        self.host_path = "bazel-out/config/bin/agent-host/persistent.cjs"
        self.host = self.execroot / self.host_path
        self.host.parent.mkdir(parents=True)
        self.host.write_text("host bundle")
        fake = self.root / "bazelisk"
        fake.write_text('''#!/bin/sh
case "$*" in
  'info execution_root') printf '%s\\n' "$EXECROOT" ;;
  'info output_base') printf '%s\\n' "$OUTPUT_BASE" ;;
  'cquery --output=files //agent-host:pinned_node') printf '%s\\n' "$NODE_PATH" ;;
  'cquery --output=files //agent-host:persistent.cjs')
    printf '%s\\n' "$HOST_PATH" "${HOST_PATH}.ok" ;;
  *) exit 2 ;;
esac
''')
        fake.chmod(0o755)
        self.makefile = self.root / "probe.mk"
        self.makefile.write_text((ROOT / "Makefile").read_text() + '''
.PHONY: probe
probe:
	@$(SDK_HOST_ENV) /bin/sh -c 'test -x "$$SDK_NODE" && test -f "$$SDK_HOST" && "$$SDK_NODE" && printf "%s\\n" "$$SDK_NODE" "$$SDK_HOST"'
''')
        self.env = dict(os.environ, PATH=f"{self.root}:/usr/bin:/bin",
                        HOME=str(self.root), EXECROOT=str(self.execroot),
                        OUTPUT_BASE=str(self.base), NODE_PATH=self.node_path,
                        HOST_PATH=self.host_path)

    def node(self, root):
        node = root / self.node_path
        node.parent.mkdir(parents=True, exist_ok=True)
        node.write_text("#!/bin/sh\nprintf 'pinned-node\\n'\n")
        node.chmod(0o755)
        return node

    def assert_resolved(self, node):
        result = subprocess.run(["/usr/bin/make", "--no-print-directory", "-f",
                                 str(self.makefile), "probe"], cwd=self.root,
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["pinned-node", str(node), str(self.host)])

    def test_external_without_execroot_link_uses_output_base(self):
        self.assert_resolved(self.node(self.base))

    def test_external_with_execroot_link_preserves_execroot(self):
        self.node(self.base)
        (self.execroot / "external").symlink_to(self.base / "external")
        self.assert_resolved(self.execroot / self.node_path)

    def test_generated_node_stays_in_execroot(self):
        self.node_path = "bazel-out/config/bin/node"
        self.env["NODE_PATH"] = self.node_path
        self.assert_resolved(self.node(self.execroot))


if __name__ == "__main__":
    unittest.main()
