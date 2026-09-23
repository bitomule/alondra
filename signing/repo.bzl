def _alondra_signing_impl(rctx):
    local = rctx.workspace_root.get_child("signing").get_child("signing.bzl")
    example = rctx.workspace_root.get_child("signing").get_child("signing.example.bzl")
    rctx.watch(local)
    rctx.watch(example)
    source = local if local.exists else example
    if not local.exists:
        print("signing/signing.bzl not found: signing with signing/signing.example.bzl")
    rctx.file("signing.bzl", rctx.read(source))
    rctx.file("BUILD.bazel", 'exports_files(["signing.bzl"])\n')

alondra_signing = repository_rule(
    implementation = _alondra_signing_impl,
    local = True,
)
