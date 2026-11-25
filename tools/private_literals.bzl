"""The private literals of this Mac, copied into the build so a test that reads them can be cached.

`ALONDRA_PRIVATE_LITERALS` names a file outside the checkout, or `none` where there is no list on
purpose. Without it the rule reads `files/private-literals.txt` in Alondra's home, which is where
a node's session finds it too. Bazel watches that file, so a test that depends on `literals.txt`
runs again only when the list or the files it checks change. The file is empty when there is no list.
"""

load(":private_literals_source.bzl", "NONE", "literals_source")

_VARIABLE = "ALONDRA_PRIVATE_LITERALS"

def _private_literals_impl(rctx):
    source = literals_source(
        rctx.getenv(_VARIABLE, ""),
        rctx.getenv("ALONDRA_HOME", ""),
        rctx.getenv("HOME", ""),
    )
    content = ""
    if source.path == NONE:
        content = NONE
    elif source.path:
        file = rctx.path(source.path)
        if file.exists:
            rctx.watch(file)
            content = rctx.read(file)
        elif source.required:
            fail("%s names a file that does not exist" % _VARIABLE)
    rctx.file("literals.txt", content)
    rctx.file("BUILD.bazel", "exports_files([\"literals.txt\"])\n")

private_literals = repository_rule(
    implementation = _private_literals_impl,
    environ = [_VARIABLE, "ALONDRA_HOME", "HOME"],
    local = True,
)
