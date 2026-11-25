"""The private literals of this Mac, copied into the build so a test that reads them can be cached.

`ALONDRA_PRIVATE_LITERALS` names a file outside the checkout, or `none` where there is no list on
purpose. Bazel watches that file, so a test that depends on `literals.txt` runs again only when the
list or the files it checks change. The file is empty when the variable is not set.
"""

_VARIABLE = "ALONDRA_PRIVATE_LITERALS"
_NONE = "none"

def _private_literals_impl(rctx):
    value = rctx.getenv(_VARIABLE, "")
    content = ""
    if value == _NONE:
        content = _NONE
    elif value:
        source = rctx.path(value)
        if not source.exists:
            fail("%s names a file that does not exist" % _VARIABLE)
        rctx.watch(source)
        content = rctx.read(source)
    rctx.file("literals.txt", content)
    rctx.file("BUILD.bazel", "exports_files([\"literals.txt\"])\n")

private_literals = repository_rule(
    implementation = _private_literals_impl,
    environ = [_VARIABLE],
    local = True,
)
