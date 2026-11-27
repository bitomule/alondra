+++
id = "antigravity-key-in-a-private-file"
title = "An Antigravity account's key lives in one private file of its folder"
status = "accepted"
date = "2026-11-27"
supersedes = []
superseded_by = ""
+++

## Context

A Gemini API key or an Agent Platform key is a credential that bills the user. Alondra needs it to
start the `agy` of the account that owns it, and nowhere else. The Mac and iPhone apps, the CLI and
the relay can all send one.

## Decision

A named account whose access takes a key keeps it in the file `gemini-api-key` of its own folder,
`<Alondra home>/provider-accounts/antigravity/<account>/`. The file is written whole: a new file
created with mode `0600`, never through a link, synced and renamed over the old one. The daemon and
the Antigravity host read it by one rule, kept in `crates/core/fixtures/antigravity-credentials.json`
that both test against: a private regular file of the user with one link, at most 4096 bytes, whose
text without the white space around it is the key (1 to 512 characters, no white space or control
character). A file the rule refuses reads as no key. The key goes into the environment of that
account's `agy` only, never into `options.toml`, the domain, a view, a reply, a receipt, a log or the
host's settings, which carry the file's path. The default account is `agy`'s own sign-in and never
keeps a key.

## Consequences

Removing an account removes its folder and its key. Changing an account's access away from one that
takes a key removes the key. A key typed in an app waits encrypted in the app's outbox until the Mac
confirms it, and nothing backs that outbox up. Every command the model runs inherits `agy`'s
environment, so a node can read its own account's key.

## Confirmation

`antigravity.key-file`, `antigravity.key` and `antigravity.apps`.
