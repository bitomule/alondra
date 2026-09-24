# Alondra

The nervous system of the hive's orchestrator: reliable state for every Claude Code node and
for the machine, structured questions that reach the user and come back to their node, and
tools for the orchestrator to act.

| Folder | What it is |
|---|---|
| `crates/core` | State model and reducer (Rust, no I/O) |
| `crates/alondrad` | 24/7 daemon: hooks, sentinel, SQLite, API |
| `crates/alondra` | CLI, TUI, the nodes' MCP server and inbox hook |
| `plugin/` | Claude Code plugin: hooks, `ask`/`report` tools and skill |
| `launchd/` | macOS service template for `alondrad` (installed by `alondra setup`) |
| `signing/` | App signing: `signing.example.bzl` → local copy `signing.bzl` and profiles |
| `docs/plan.md` | Design and phased plan |

## Trying it

On a new Mac, follow [`INSTALL.md`](INSTALL.md) (written for an agent to execute).

```bash
make install        # builds with Bazel and installs alondra and alondrad in ~/.local/bin
alondra setup       # config.toml, launchd service, plugin and iPhone pairing (--help, --dry-run)
alondra doctor      # what works and what is missing, with how to fix it
alondra             # the TUI
alondra status      # the same as text
```

A node connected to Alondra is launched with the plugin:

```bash
claude --bg --name "test/alondra" --plugin-dir ~/Projects/alondra/plugin --permission-mode auto -- "<mission>"
```

or, from the orchestrator, with its `launch` tool.

Settings live in `~/.alondra/config.toml` (`name`, `port`, `listen`, `public_url`,
`legacy_bridge`, `wake`, `retention_hours`, `service_label`). An `ALONDRA_*` environment
variable overrides the file; `alondra config` shows the values in effect.

**The orchestrator** is started with `--plugin-dir ~/Projects/alondra/plugin-orchestrator`: the
same hooks plus the `fleet`, `node`, `machine`, `send` and `launch` tools. Alondra notifies it
on its own, every 90 s and in a single message, of:
- new reports from the nodes;
- questions addressed to it;
- nodes that finish without saying what they want;
- the machine, when it runs out of memory or gets saturated.

(`claude --bg` does not inherit environment variables, so the role goes in its own plugin.)

If no orchestrator is running, alondrad launches one this way on its own and posts a notice
saying so (only when `wake` is on).

### The TUI

- **Waiting for you**: questions from the nodes (structured, menu, permissions and plans).
  `Enter` opens them with all their context, `1`–`4` picks an option and `r` answers with text.
- **Fleet**: everything `claude agents` lists, with its state (working, waiting for an answer,
  delivering a result, waiting for the machine, done, unclear what it wants). `~` = reported by
  the Claude Code daemon, `*` = decided by jevi.
- `m` sends a message to the selected node and `o`, to the orchestrator.

## Development

`make build` / `make test` with Bazel. `cargo build` / `cargo test` also work, for fast
iteration and for rust-analyzer.
