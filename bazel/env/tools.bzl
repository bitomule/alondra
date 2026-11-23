"""The programs Alondra's checks and snapshot tests need besides a toolchain, fetched by Bazel.

Someone who clones the repository installs none of them by hand: Bazel downloads each from its
release page, checks the hash below and keeps it in the output base, and `//bazel/env:bazel_env`
puts the commands on PATH (README.md). This file is the one place that names their versions. Bump a
tool by changing its url and sha256. Nothing here is downloaded until something needs it.
`//tools:ci_test` fails when the pull request workflow, which installs `musts` without Bazel, pins
another version or hash.
"""

load("@bazel_tools//tools/build_defs/repo:http.bzl", "http_archive", "http_file")

# musts validates and records the checks of `MUSTS.yml`. The pull request workflow runs it on a Linux
# runner without Bazel, so it keeps its own copy of the version and the Linux hash, which the test
# above compares with these.
MUSTS_VERSION = "0.10.1"
MUSTS_SHA256 = {
    "aarch64-apple-darwin": "0b465a5e8742ea0be1938bac5c745fa5cad1b5a23c7170e1aee2b1ea0b54e31d",
    "x86_64-unknown-linux-gnu": "f0407f69edb8095885e70ab2ffeeed90ea83ed013ffe41ece178c67acf232bdc",
}

def _simpool_env_impl(rctx):
    # A test action has no PATH and its working directory is a runfiles tree: the runner needs the
    # binary's absolute path in the output base, which lives as long as this repository does.
    binary = rctx.path(rctx.attr.binary)
    rctx.file("BUILD.bazel", "exports_files([\"env.bzl\"])\n")
    rctx.file("env.bzl", "SIMPOOL_TEST_ENV = {\"SIMPOOL_BIN\": %s}\n" % repr(str(binary)))

_simpool_env = repository_rule(
    implementation = _simpool_env_impl,
    attrs = {"binary": attr.label(allow_single_file = True, mandatory = True)},
)

def _developer_tools_impl(module_ctx):
    # simpool leases the iOS simulators the snapshot tests run on.
    http_file(
        name = "simpool_bin",
        downloaded_file_path = "simpool",
        executable = True,
        sha256 = "c55fbeda49955cf9c1911be4477848c66156496a9e4abc74edd2da30af5f0d8a",
        urls = ["https://github.com/bitomule/simpool/releases/download/v0.35.0/simpool-darwin-arm64"],
    )

    # simpool's test runner (`simpool_ios_test_runner`), from the same release as simpool_bin; bump
    # both together. An archive of the commit the tag names, not a git repository: under Bazel 8.4.2,
    # git_repository's strip_prefix leaves absolute symlinks into the output base that fetched it.
    http_archive(
        name = "simpool",
        sha256 = "cca7b3d0d7ff23cf3c2e3f1d32fd7ae4c13556432520d6bd17c7e956cad6f6a0",
        strip_prefix = "simpool-993ec8233acdd514eb72662e05e67dd9eb8b2468/bazel",
        urls = ["https://github.com/bitomule/simpool/archive/993ec8233acdd514eb72662e05e67dd9eb8b2468.tar.gz"],  # v0.35.0
    )
    _simpool_env(name = "simpool_env", binary = "@simpool_bin//file:simpool")

    http_archive(
        name = "musts",
        build_file_content = 'exports_files(["musts"])',
        sha256 = MUSTS_SHA256["aarch64-apple-darwin"],
        strip_prefix = "musts-aarch64-apple-darwin",
        urls = ["https://github.com/bitomule/musts/releases/download/musts-v{}/musts-aarch64-apple-darwin.tar.xz".format(MUSTS_VERSION)],
    )

    return module_ctx.extension_metadata(reproducible = True)

developer_tools = module_extension(implementation = _developer_tools_impl)
