# Antigravity

How Alondra runs nodes on Antigravity accounts, in plain words. The specs under
`specs/antigravity/` are the source of truth; where this page disagrees with one, the spec wins.
The specs state what Alondra does today. Where a decision fixes a behaviour that is not built yet,
this page says so and names the decision; its scenarios arrive with the change that builds it.

## What the integration is
<!-- section: antigravity.overview explains: antigravity.boundary, boundary:antigravity, decision:antigravity-reaches-google-through-agy -->

Antigravity is Google's coding agent. Its command-line program, `agy`, speaks the Agent Client
Protocol (ACP) on standard input and output. Alondra runs a node on an Antigravity account by
starting an Antigravity host, a process of its own that starts `agy acp` and translates between ACP
and the host protocol every Alondra host speaks. The orchestrator never runs on Antigravity.

Alondra learns everything about an Antigravity account from `agy` itself: its version, its ACP
server and its `/usage` report. It never calls Google's internal endpoints, because a client that
does can get the account flagged. Today Alondra makes no network call of its own. The decision
`antigravity-reaches-google-through-agy` allows one, not built yet: to Google's public Gemini API,
to check a Gemini API key when it is saved and once a day.

## Accounts, access and the key
<!-- section: antigravity.accounts-and-keys explains: antigravity.accounts, antigravity.key-file, antigravity.key, decision:antigravity-key-in-a-private-file, decision:antigravity-key-change-reaches-running-nodes -->

An Antigravity account reaches Google in one of four ways, its access: `google` (agy's own Google
sign-in), `enterprise` (a Gemini Enterprise sign-in with a project and a location), `api_key` (a
Gemini API key) or `cloud_project` (a Google Cloud project, with an Agent Platform key or the Mac's
application default credentials). The default account is agy's own sign-in in `~/.gemini`. Every
other account is named and gets a private folder,
`<Alondra home>/provider-accounts/antigravity/<account>/`, which links the user's own rules, skills
and settings from `~/.gemini` but keeps its own sign-in. Alondra never signs in for the user: the
accounts screen shows the command that does.

A key account keeps its key in the file `gemini-api-key` of its folder. Alondra writes it whole,
private to the user, never through a link. The daemon and the host read it by one rule that both
test against, so a file is the same key, or no key, wherever it is read. The key reaches only that
account's `agy`, through its environment, and appears nowhere else: not in `options.toml`, the
domain, a view, a reply, a receipt or a log. Changing an account's access away from one that takes
a key removes the key, and removing the account removes its folder.

A removed key starts no later `agy`. The decision `antigravity-key-change-reaches-running-nodes`
goes further, and is not built yet: removing a key will stop the nodes running on the account at
once, and replacing it will let each running turn end and give the next turn the new key.

## Checking an account, its models and its usage
<!-- section: antigravity.checks explains: antigravity.account-check, antigravity.models, antigravity.usage, decision:antigravity-quota-holds-its-group, decision:antigravity-refused-key-signs-the-account-out, decision:antigravity-one-version-rule -->

The account check runs `agy --version` and then `agy acp` without a prompt: it initializes, signs
in with the account's key when the access takes one, opens a session to read the models and
thinking levels it offers, and closes it. It never starts a Google sign-in. An account checks
signed in, signed out, or unavailable with a reason (agy is missing, too old, or another agent). A
signed-in result stands for a day and any other result for an hour; changing the account, its key
or the `agy` on the Mac checks it again at once. The lowest version Alondra runs is `1.5.12`. The
decision `antigravity-one-version-rule`, not built yet, makes the check and the host read a version
by one rule, its numbers, so that `1.5.14-beta` counts as `1.5.14` for both.

The decision `antigravity-refused-key-signs-the-account-out`, not built yet, checks a Gemini API key
against Google's public Gemini API when it is saved and in the daily check: a key Google refuses
will sign the account out, and a key Google refuses during a turn will hold the task like a refused
sign-in.

A Google-signed-in account reads its usage with `agy -p /usage`. Each quota pool becomes one
window. The account stops taking tasks only when every pool ran out, and comes back at the soonest
reset. Accounts billed per use read no usage. The decision `antigravity-quota-holds-its-group`, not
built yet, makes a turn that ends because its group's quota ran out wait for that group's reset and
then go on by itself, while the account keeps taking tasks whose models have room.

## A node's session
<!-- section: antigravity.sessions explains: antigravity.host-opening, antigravity.sessions, antigravity.permissions, antigravity.turns, decision:antigravity-handoff-when-resume-fails, decision:antigravity-blocked-tools-hold-the-account -->

The host checks the `agy` it started before it offers any input, builds agy's environment from the
account alone (inherited Google credentials are always removed), gives agy Alondra's MCP server so
the node can deliver, and passes the node's instructions with the session when agy accepts them
there, or ahead of the first prompt otherwise. It then applies the node's model and thinking level.
Anything that keeps the session from opening refuses the input as never offered, with a code that
says why.

The host remembers which ACP session it opened, in a private file, and a later host of the same
node resumes it, so the conversation goes on where it was. The decision
`antigravity-handoff-when-resume-fails`, not built yet, starts a new session with the task's
handoff ahead of the next input when `agy` no longer holds the old one. The host sends one prompt at
a time; an input offered during a turn waits for the turn to end.

When `agy` asks a permission, the host turns it into a decision for the user, unless the session's
mode answers it by itself, and answers with exactly one of `agy`'s own options, never an "always"
one: "allow once" or "reject once" for a permission, the chosen option for a question `agy` asks
with options of its own. A question the user denies, and any request an interrupt ends, is answered
"cancelled". In auto mode, a call to one of Alondra's own tools is allowed without asking; in plan
mode it asks like any other. The decision `antigravity-blocked-tools-hold-the-account`, not built
yet, holds the task of an account whose `agy` does not start Alondra's tools on a card, and gives
the account no more nodes, because its nodes could not deliver.

A signed-out account holds its task on the card "Sign in to Antigravity outside Alondra" and starts
no other host until the user signs in and tries again.

## What is measured and what is assumed
<!-- section: antigravity.measured explains: antigravity.conformance -->

The conformance transcripts and the fake `agy` the tests run are written by hand. These facts about
a real `agy` were measured: upstream errors from Google, including a refused key, arrive as the text
of a turn that ends with `end_turn`, not as an error; `authenticate` with `gemini-api-key` accepts
any key; `session/resume` keeps the conversation across processes; the `_meta` system prompt append
reaches the model; and `usage_update` reports the context used. These are still assumptions that
the fake encodes: an `auto` session mode, a tool call's `name` shaped `mcp_alondra_<tool>`, errors
with structured `data`, `agent-platform` authenticating without a browser, and the exact shape of a
used-up quota. A spec that rests on one of them says "when agy …".
