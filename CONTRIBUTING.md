# Contributing to Alondra

Thanks for helping. Read this before opening a pull request.

## Licensing of contributions

Alondra is under the Elastic License 2.0 (see `LICENSE.md`). By sending a contribution you agree
that it is released under that same license. There is no separate contributor agreement and no
copyright assignment: you keep your copyright.

## Where things go

- **Pull requests** go directly to this repository (`bitomule/alondra`).
- **Bugs and feature requests** open an issue here, with the templates.
- **Hangs, crashes and tasks that do not move** go to
  [bitomule/alondra-issues](https://github.com/bitomule/alondra-issues), which asks for the diagnostics.
- **Security problems** never go in a public issue: follow [`SECURITY.md`](SECURITY.md).
- A big feature starts as an issue, so we agree on the approach before you write it.

Everything you commit, comment or attach to a pull request is public and stays in git history. Do
not include secrets, personal data (real emails, home paths such as `/Users/<name>`, device or account
ids, public IP addresses), names of private repositories or apps, unpublished security problems or
internal reports. Use synthetic values such as `ana@example.com` and `/Users/ana`, and commit with
your GitHub `noreply` email. `//tools:public_data_guard_test` fails when a tracked file carries such
data.

## Build and test

Build and test **only through Bazel**, never `cargo test`, `swift test` or `xcodebuild`:

```bash
make build        # bazelisk build //crates/... //apple:AlondraMac
make test         # bazelisk test //crates/...
```

Apple tests, web checks and the rest are in [`docs/running-tests.md`](docs/running-tests.md); how to
build from a clean Mac is in [`docs/building-from-source.md`](docs/building-from-source.md).

## What a pull request needs

- **A test that fails without your change and passes with it.** Show it in the pull request:
  remove the change, see the test fail, put it back, see it pass.
- **No flaky tests**: no wall-clock assertions, no sleeps racing other work, no retries that hide
  failures.
- **Tests clean up after themselves**: they use temporary directories that are removed on success
  and failure, and never touch your real `~/.alondra`, Keychain or accounts.
- **Screens come with captures.** A change to what the user sees shows its captures in the pull
  request description: light and dark, and every changed language.
- **Strings in English**, with the Spanish translation in the existing localization files.
- **Stored data keeps decoding**: new fields have defaults and enums only grow.
- **No decision reads prose.** An error message, process output or model text never picks a
  branch; use a type, a status code or a documented field.
- **No comments that restate the code**, and no version in the name of a new type or file.

## Pull request flow

1. Fork, branch from `main`, keep the change small and focused. Name the branch and title the pull
   request by what changes (`Reselect disabled providers before first launch`); link the issue it
   closes as `Closes #N` in the description.
2. Make `make build` and `make test` pass; run `musts validate` if you have `musts` installed. It asks
   only for the checks whose files your change touches, and a change to prose or images no test
   loads asks for nothing. Adding a test or fixture? Run `make musts-scopes` to keep the check's
   `paths` complete.
3. Open the pull request with the template filled in.
4. A maintainer reviews it. Nothing under `.github/workflows` is ever added: releases run on the
   maintainer's Mac, not on hosted runners.

## Conduct

Be kind. The [Code of Conduct](CODE_OF_CONDUCT.md) applies everywhere the project is discussed.
