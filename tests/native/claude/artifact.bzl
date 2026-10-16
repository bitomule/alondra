"""Lazy artifact for the opt-in native harness, separate from the SDK npm graph."""

_NATIVE_ARCHIVE_URL = "https://registry.npmjs.org/@anthropic-ai/claude-agent-sdk-darwin-arm64/-/claude-agent-sdk-darwin-arm64-0.3.300.tgz"
_NATIVE_ARCHIVE_INTEGRITY = "sha512-+pFKo4x5S6j4v+UTVZZFiLoJO7PMLVPNhIUBalk/RvMhcaN33Ubgk4aRAZpJmkxCcGMN6rWCevYhgT0nXkqytg=="

def _native_test_archive_impl(ctx):
    ctx.download(url = _NATIVE_ARCHIVE_URL, output = "native-package.tgz", integrity = _NATIVE_ARCHIVE_INTEGRITY)
    ctx.file("BUILD.bazel", """
filegroup(
    name = "archive",
    srcs = ["native-package.tgz"],
    testonly = True,
    visibility = ["//visibility:public"],
)
""", executable = False)

native_test_archive = repository_rule(implementation = _native_test_archive_impl)
