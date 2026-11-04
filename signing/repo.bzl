def _signing_dir(rctx):
    configured = rctx.getenv("ALONDRA_SIGNING_DIR")
    if configured:
        return rctx.path(configured)
    return rctx.path(rctx.getenv("HOME") + "/.alondra/signing")

_PLACEHOLDER_TEAM_ID = 'TEAM_ID = "ABCDE12345"'

def _team_id_line(rctx, profiles, directory):
    for profile in sorted(profiles):
        decoded = rctx.execute(["security", "cms", "-D", "-i", str(directory.get_child(profile))])
        if decoded.return_code != 0:
            continue
        rctx.file("profile.plist", decoded.stdout)
        team = rctx.execute(["plutil", "-extract", "TeamIdentifier.0", "raw", "-o", "-", "profile.plist"])
        rctx.delete("profile.plist")
        if team.return_code == 0 and team.stdout.strip():
            return 'TEAM_ID = "%s"' % team.stdout.strip()
    return _PLACEHOLDER_TEAM_ID

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
    source = rctx.read(local if local.exists else example)
    if not [line for line in source.splitlines() if line.startswith("TEAM_ID")]:
        source += "\n" + _team_id_line(rctx, profiles, directory) + "\n"
    rctx.file("signing.bzl", source)
    rctx.file("BUILD.bazel", "exports_files([\"signing.bzl\"] + {})\n".format(repr(profiles)))

alondra_signing = repository_rule(
    implementation = _alondra_signing_impl,
    local = True,
    environ = ["ALONDRA_SIGNING_DIR", "HOME"],
)
