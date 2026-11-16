"""The checked-in files of a package and of every package below it, as one declared input.

A test that reads the checkout lists the `tree` of the directories it reads in its `data`, so it reads
only declared files from its runfiles and Bazel reruns it exactly when one of them changes. Every
package calls `repo_tree()`: a package without one breaks the analysis of the trees above it.
"""

# The local state .gitignore keeps out of git, so a worktree's scratch never becomes a test input.
LOCAL_STATE = [
    ".git",
    ".git/**",
    ".claude/**",
    ".scratch/**",
    ".musts/state.sqlite*",
    ".musts/.lock*",
    ".musts/evidence/**",
    ".musts/bep.json",
    ".bazelrc.local",
    ".bazelrc.local.*",
    ".bazel-output-disk.log",
    ".bazel-test-tmp/**",
    ".beta-build/**",
    ".e2e-captures/**",
    ".bazel_vendored/**",
    ".build/**",
    ".bsp/skbsp_generated/**",
    ".bazel-links/**",
    "bazel-*",
    "bazel-*/**",
    "**/target/**",
    "**/node_modules/**",
    "**/__pycache__/**",
    "**/.DS_Store",
]

def repo_tree(exclude = []):
    """Declares `:files`, this package's own files, and `:tree`, those plus the `:tree` below it."""
    package = native.package_name()
    prefix = "//" + package + ("/" if package else "")
    native.filegroup(
        name = "files",
        srcs = native.glob(["**"], exclude = LOCAL_STATE + exclude, allow_empty = True),
        visibility = ["//visibility:public"],
    )
    native.filegroup(
        name = "tree",
        srcs = [":files"] + [
            prefix + sub + ":tree"
            for sub in native.subpackages(include = ["**"], exclude = LOCAL_STATE, allow_empty = True)
        ],
        visibility = ["//visibility:public"],
    )
