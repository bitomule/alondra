# Security policy

## Reporting a vulnerability

Do not open a public issue. Report it privately, either way:

- **GitHub:** use "Report a vulnerability" under the Security tab of this repository.
- **Email:** [security@getalondra.com](mailto:security@getalondra.com).

Include what you found, how to reproduce it and the version (`alondra --version`). We aim to
answer within a few days, tell you what we decide, and credit you when the fix ships if you wish.

## Scope

In scope: the daemon, CLI and TUI, the iPhone and Mac apps, the end-to-end protocol between devices
and a Mac (`docs/iroh-protocol.md`), and the control-plane server in this repository.

Out of scope: Claude Code, Codex and other provider tools, and GitHub. Report those to their
owners. Alondra does not manage provider sign-in and never holds those credentials.

## Supported versions

Only the latest release receives fixes.
