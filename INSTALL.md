# Installing Alondra

Before installing, read [what Alondra does on your Mac](docs/on-your-mac.md). It inventories
programs, files, connections, macOS permissions and credentials, cites their source, and includes
a prompt for an agent to audit the checkout without running it.

```bash
brew install bitomule/tap/alondra
alondra
```

`alondra` walks you through the rest: the folder with your projects, the relay for
notifications, the background service and the Claude Code plugin. Run it again any time; it only
changes what is missing.

Needs a Mac on Apple Silicon with macOS 26 or later and Claude Code 2.1.321 or later, signed in.
Alondra brings everything else it runs, Node and the Agent SDK included.

## The apps

The iPhone and Mac apps are in alpha and come with access to Alondra Cloud (join the waiting list at
getalondra.com), through TestFlight. Open the app on the
same Wi-Fi as your Mac: it finds the Mac, both screens show the same code, and you allow it on
the Mac (in the Mac app or the TUI).

## If something fails

```bash
alondra doctor          # what works and what is missing, with the command that fixes each
alondra doctor --fix    # repairs what it can
```

`alondra update` installs the newest release. Logs are in `~/.alondra/alondrad.log`; past 10 MB it moves to `alondrad.log.1`, replacing the previous one. What each node's Alondra tools sent and how long each answer took is in `~/.alondra/mcp-calls.log` (past 2 MB it moves to `mcp-calls.log.1`).

Building from the source, using your own Apple developer account, or Tailscale for access from
outside home: [`docs/building-from-source.md`](docs/building-from-source.md).
