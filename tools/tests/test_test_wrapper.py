#!/usr/bin/env python3
"""The tests of this run ran under the repository's wrapper, so its guard refused every undeclared exec."""
import os
import unittest

LINE = "try-import %workspace%/tools/test-wrapper.bazelrc"


class TestWrapper(unittest.TestCase):
    def test_this_run_uses_the_repositorys_wrapper(self):
        if os.environ.get("ALONDRA_TEST_WRAPPER") != "1":
            self.fail(
                "The tests of this run ran under another --run_under than //tools:test_tmpdir, so none of "
                "them was guarded against running programs they do not declare. A --run_under in your own "
                "~/.bazelrc overrides the repository's: add this as the last line of it, which changes "
                f"nothing in other repositories:\n\n    {LINE}\n"
            )


if __name__ == "__main__":
    unittest.main()
