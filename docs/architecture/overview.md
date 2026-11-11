# Architecture overview

How Alondra's pieces fit. Each section names the code that owns it.

## The picture

```
  iPhone app ─┐                                   ┌─ Claude Agent SDK host ─ Claude Code
  Mac app ────┼─ end-to-end sealed envelopes ─▶ alondrad ─┤
  TUI / CLI ──┘   (direct or relay)               │       └─ Codex host ─ Codex
                                                  │
                                   SQLite: domain state, journals, schedules
```

Everything that does work runs on your Mac. The only server in the picture is an optional control
plane (accounts, pairing, push, relay) that never sees the contents of what devices exchange.

## The daemon: `alondrad`

`crates/alondrad` runs 24/7 under launchd. It owns the state, launches and watches the agent
sessions, talks to GitHub, and serves every client. A newer binary replaces the running one by
itself: the new binary first replays the stored state on a private copy (`alondrad --preflight`),
and only if that passes does the old daemon exit and launchd start the new version.

## Domain: requests, tasks, nodes and cards

- A **request** is what you ask for. The **orchestrator** turns it into a plan and **tasks**.
- A **task** owns one piece of work: its worktree, its pull requests and its state
  (waiting for another task, running, needs you, in review, closing, done, cancelled). Each pull
  request reaches you on its own card, and the task closes once none is open and one merged.
- A **node** is the agent session that does a task. One node per task; it closes with the task.
- A **card** is something that needs you: a question, a plan, a permission, a delivery. Each card
  is decided from itself alone.

The domain changes only by committing a command. `crates/domain` holds the lifecycle rules,
`crates/core` the state model and reducer, and `crates/store` the transactional journal. Stored
formats keep decoding old data: new fields have defaults and enums only grow. Lifecycle changes
keep replaying journals recorded before them.

## Providers

A provider is a way to run a node. `crates/provider-contract` is the technical contract;
`crates/provider-claude` and `crates/provider-codex` implement it, through the Agent SDK host for
Claude and the Codex app server for Codex (`agent-host/`). A node uses the login the provider's
own tool already holds on the Mac: Alondra never sees or stores provider credentials, and several
accounts of each provider can be configured.

The daemon and a host speak a versioned protocol. A new action names a new capability; changing
an existing frame takes a new protocol version that the daemon speaks beside the old one, so an
upgrade never strands a running host.

## The read side

Screens read a committed snapshot, never the domain being changed. Each commit builds a read
index once and publishes it with the state as one revision; the TUI, the apps, the changes feed
and push content all read that snapshot plus a live snapshot of what nodes, hooks and GitHub
report. [`read-side.md`](read-side.md) has the shape and the rules.

## Devices

Clients are the TUI and CLI (`crates/alondra`) and the iPhone and Mac apps (`apple/`). A device is
paired with a Mac once, in person: the Mac shows a code, the device pins the Mac's keys and the
Mac approves the device's keys. After that every request and response is a sealed envelope
(`crates/e2e`): encrypted and signed end to end, with the route, sender, recipient and expiry
authenticated. Whatever carries the envelope (a direct connection or the relay) sees
opaque bytes.

[`../iroh-protocol.md`](../iroh-protocol.md) describes the transport devices use to reach a Mac.
Devices on the same network connect directly; away from home they try a direct path and fall back
to the relay.

## The control plane

`crates/relay` and `web/` are the optional server: accounts, a device registry,
pairing invitations, push notifications through APNs and the admission check for the iroh relay.
It stores accounts and device public keys, not content.

A relay's `GET /v1/bootstrap` names the protocol, the epoch devices sign with, the iroh relay, and
in `identity` the OIDC issuer whose tokens it accepts with the client id of the apps
(`apple_client_id`) and of the CLI (`cli_client_id`). The apps and the CLI are built for one relay
and one issuer, held in constants.

## Where to look

| Question | Start at |
|---|---|
| How does a task move between states? | `crates/domain`, `crates/core` |
| What does a task show, and what keeps it from standing still? | [`task-states.md`](task-states.md) |
| How are screens fed? | [`read-side.md`](read-side.md) |
| How does a device reach a Mac? | [`../iroh-protocol.md`](../iroh-protocol.md) |
| How do I build or test it? | [`../building-from-source.md`](../building-from-source.md), [`../running-tests.md`](../running-tests.md) |
