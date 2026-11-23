"""The programs Alondra's checks and snapshot tests need besides a toolchain, fetched by Bazel.

Someone who clones the repository installs none of them by hand: Bazel downloads each from its
release page, verifies the hash below and keeps it in the output base. This file is the one place
that names their versions. `tools/tests/test_ci.py` fails when the pull request workflow, which
installs `musts` without Bazel, pins another version or hash.
"""

load("@bazel_tools//tools/build_defs/repo:http.bzl", "http_archive", "http_file")

# simpool leases the iOS simulators the snapshot tests run on. The Bazel runner rule
# (`simpool_ios_test_runner`, `archive_override` in MODULE.bazel) and the binary it starts come from
# the same release: bump the archive's tag, integrity and prefix together with the values here.
SIMPOOL_VERSION = "0.35.0"
SIMPOOL_DARWIN_ARM64_SHA256 = "c55fbeda49955cf9c1911be4477848c66156496a9e4abc74edd2da30af5f0d8a"

# musts validates and records the checks of `MUSTS.yml`. The pull request workflow runs it on a Linux
# runner without Bazel, so it keeps its own copy of the version and the Linux hash, which the test
# above compares with these.
MUSTS_VERSION = "0.10.1"
MUSTS_SHA256 = {
    "aarch64-apple-darwin": "0b465a5e8742ea0be1938bac5c745fa5cad1b5a23c7170e1aee2b1ea0b54e31d",
    "x86_64-unknown-linux-gnu": "f0407f69edb8095885e70ab2ffeeed90ea83ed013ffe41ece178c67acf232bdc",
}

def _simpool_env_impl(rctx):
    # An action's PATH is empty and its working directory is a runfiles tree: the runner needs the
    # binary's absolute path in the output base, which lives as long as this repository does.
    binary = rctx.path(rctx.attr.binary)
    rctx.file("BUILD.bazel", "exports_files([\"env.bzl\"])\n")
    rctx.file("env.bzl", "SIMPOOL_TEST_ENV = {\"SIMPOOL_BIN\": %s}\n" % repr(str(binary)))

_simpool_env = repository_rule(
    implementation = _simpool_env_impl,
    attrs = {"binary": attr.label(allow_single_file = True, mandatory = True)},
)

def _pinned_tools_impl(module_ctx):
    http_file(
        name = "simpool_bin",
        urls = ["https://github.com/bitomule/simpool/releases/download/v{0}/simpool-darwin-arm64".format(SIMPOOL_VERSION)],
        sha256 = SIMPOOL_DARWIN_ARM64_SHA256,
        executable = True,
        downloaded_file_path = "simpool",
    )
    _simpool_env(name = "simpool_env", binary = "@simpool_bin//file:simpool")

    http_archive(
        name = "musts",
        urls = ["https://github.com/bitomule/musts/releases/download/musts-v{0}/musts-aarch64-apple-darwin.tar.xz".format(MUSTS_VERSION)],
        sha256 = MUSTS_SHA256["aarch64-apple-darwin"],
        strip_prefix = "musts-aarch64-apple-darwin",
        build_file_content = """
load("@bazel_skylib//rules:native_binary.bzl", "native_binary")

exports_files(["musts"])

native_binary(
    name = "bin",
    src = "musts",
    out = "musts_bin",
    visibility = ["//visibility:public"],
)
""",
    )
    return module_ctx.extension_metadata(reproducible = True)

pinned_tools = module_extension(implementation = _pinned_tools_impl)
