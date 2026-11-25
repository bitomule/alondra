"""The sources a crate ships: everything under `src` but the files only its tests compile.

A test file that is an input of a production target rebuilds or reruns everything that depends on
that target, although no binary changes (docs/architecture/build-graph.md).
"""

# Test modules by name: `mod tests;`, `mod <name>_tests;` and anything under a `tests` folder.
TEST_SOURCES = ["src/**/tests.rs", "src/**/*_tests.rs", "src/**/tests/**"]

def production_sources(srcs = ["src/**/*.rs"], test_only = [], exclude = []):
    """The files of `srcs` that production code compiles.

    Args:
      srcs: glob patterns of the crate's sources.
      test_only: files compiled only under `#[cfg(test)]` whose name does not say so.
      exclude: other files the target leaves out, such as a binary's `src/main.rs`.

    Returns:
      The matching files, without TEST_SOURCES, `test_only` and `exclude`.
    """
    return native.glob(srcs, exclude = TEST_SOURCES + test_only + exclude)
