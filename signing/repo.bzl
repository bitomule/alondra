def _signing_dir(rctx):
    configured = rctx.getenv("ALONDRA_SIGNING_DIR")
    if configured:
        return rctx.path(configured)
    return rctx.path(rctx.getenv("HOME") + "/.alondra/signing")

def _alondra_signing_impl(rctx):
    directory = _signing_dir(rctx)
    example = rctx.workspace_root.get_child("signing").get_child("signing.example.bzl")
    rctx.watch(example)
    local = directory.get_child("signing.bzl")
    profiles = []
    if directory.exists:
        rctx.watch_tree(directory)
        for entry in directory.readdir():
            if entry.basename.endswith((".mobileprovision", ".provisionprofile")):
                profiles.append(entry.basename)
                rctx.symlink(entry, entry.basename)
    if local.exists:
        rctx.file("signing.bzl", rctx.read(local))
    else:
        rctx.file("signing.bzl", rctx.read(example))
    rctx.file("BUILD.bazel", "exports_files([\"signing.bzl\"] + {})\n".format(repr(profiles)))

alondra_signing = repository_rule(
    implementation = _alondra_signing_impl,
    local = True,
    environ = ["ALONDRA_SIGNING_DIR", "HOME"],
)
