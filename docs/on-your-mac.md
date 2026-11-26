# What Alondra does on your Mac

This inventory describes the shipped daemon, CLI, agent hosts and Mac app. Paths below use
`~/.alondra` for the default state folder; `ALONDRA_HOME` can move it. Homebrew paths use
`<prefix>` (normally `/opt/homebrew`). Optional features are marked explicitly. Links point to
the implementation so you can check a release before installing it.

An agent runs with your OS account and the tools, settings and integrations you give its
provider. This is an inventory of Alondra's own behavior, not an allowlist of everything an
agent can do. Provider requests can include your prompts and code; configured MCP servers,
repository hooks and commands can access more files and services.

## Programs installed and started

| Program or component | When it runs and what it does | Source |
|---|---|---|
| `alondra`, `alondrad`, `alondrad-launcher` | Homebrew installs these in `<prefix>/bin` through its Cellar/opt links. The CLI opens the TUI, performs setup and serves MCP. The daemon owns tasks and devices. The launcher starts the daemon. | [Formula](../deploy/brew/alondra.rb.in), [setup](../crates/cli/src/setup.rs), [context readings](../crates/engine-live/src/contexts.rs) |
| `~/.alondra/bin/alondrad-launcher` | Setup copies the bundled launcher to this stable path. launchd starts it at login and keeps the daemon running. The default job is `dev.alondra.alondrad`, in `~/Library/LaunchAgents/dev.alondra.alondrad.plist`; the label is configurable. The plist supplies PATH, HOME, Alondra home and selected provider profile paths, and sends stdout/stderr to the daemon log. | [Launcher](../crates/host/src/launcher.rs), [plist template](../launchd/alondra.plist.in) |
| `alondra-host`, `alondra-codex-host`, `alondra-antigravity-host` | Bundled executables under `<prefix>/opt/alondra/libexec/alondra/{sdk,codex,antigravity}` contain Node and Alondra's host code. Hosts run for active agent sessions, with private Unix sockets and journals; they can survive a daemon restart. A node that rests two minutes with nothing in its host has its host and every process under it ended, and a new host resumes the session when the task has work. | [Backend selection](../crates/engine-kernel/src/sdk_backend.rs), [host ownership](../crates/agent-runtime/src/owned_host.rs), [artifact verification](../crates/foundation/src/artifact_digest.rs) |
| Your `claude` or `codex` | The Claude Agent SDK host launches your installed Claude Code. The Codex host launches `codex app-server --listen stdio://`. Account/model/MCP probes also start these tools. Before a host starts on your Claude Code or Codex, the daemon runs `claude --version` or `codex --version` once per binary, through the hosts' owner, in a process group of its own that is killed after 20 seconds, and refuses a release it cannot run. Provider settings can add hooks, plugins and MCP subprocesses. When your Claude Code is older than the release Alondra needs, the daemon, `alondra update` and `alondra doctor --fix` run its own updater (`claude update`, or `brew upgrade --cask claude-code` for a Homebrew cask) with a five-minute limit, but never while a running process holds the binary an in-place install would replace (`ps -axo comm=` tells), and say the updater's own reason when it refuses. | [Claude session](../agent-host/session.ts), [Codex session](../agent-host/codex-session.ts), [probes](../crates/daemon-engine-machine/src/native_probe.rs), [version check](../crates/engine-kernel/src/sdk_backend.rs), [Claude Code update](../crates/host/src/claude_update.rs) |
| Your `agy` (Antigravity CLI) | The Antigravity host of a node on an Antigravity account starts `agy acp` (with `--gemini-dir <folder>` for a named account) in the task's worktree, in the host's own process group, and speaks the Agent Client Protocol with it over stdio until the node rests or ends; `agy` then runs its tools and the hooks and MCP servers of your settings, as it does in your terminal, plus Alondra's MCP server. Its environment is the node's, built the same way as the checks' below, with the account's key added for a key account. Alondra finds `agy` on the `PATH` its nodes get and refuses one older than 1.5.12. It starts `agy` for an account check when the account is added, when its key or access changes, when you ask for a check, when `agy` is installed or updated, after the daemon starts, and then once a day for a signed-in account or once an hour for one signed out or that could not be checked (an API-key account with no key reads signed out without starting it): `agy --version`, then `agy acp`, where it sends `initialize`, `authenticate` for a key or Cloud project account (never a Google sign-in), opens one session in an empty folder, selects each model in turn to read its thinking levels, and closes it. No prompt is sent, so no quota is spent. A Google or Enterprise account's quota is then read with `agy -p "/usage" --output-format json`, which starts no turn. Each of these processes gets an environment built from scratch: no inherited Google credential or project variable, `AGY_CLI_DISABLE_AUTO_UPDATE=1`, and a `TMPDIR` of its own that is removed when it ends. `alondra doctor` runs `agy --version`. | [Antigravity accounts](../crates/system/src/antigravity_account.rs), [environment](../crates/core/src/antigravity_account.rs), [checks](../crates/daemon-engine/src/antigravity_accounts.rs) |
| Alondra node and orchestrator plugins | Homebrew ships the marketplace and plugins under `libexec/alondra`. The daemon maintains copies in `~/.alondra/plugin` and `~/.alondra/plugin-orchestrator`, including a wrapper pointing at its CLI. Claude setup registers the local marketplace and disables older globally enabled Alondra plugins; Alondra loads the relevant plugin for its own sessions. Node hooks call `alondra hook`; MCP starts `alondra mcp-node` or `alondra mcp-orchestrator`. | [Plugin installation](../crates/system/src/plugins.rs), [node hooks](../plugin/hooks/hooks.json), [node mod](../plugin/hooks/register.ts), [MCP](../plugin/.mcp.json), [orchestrator MCP](../plugin-orchestrator/.mcp.json) |
| `diskutil`, `plutil` | For configured or recorded external worktree storage, read the volume identity (`diskutil info -plist`, decoded with `plutil`) and mount that saved volume with `diskutil mount` if unavailable. No volume is created and no sudo is requested. | [Worktree storage](../crates/system/src/worktrees_root.rs) |
| `git`, `gh` | Create, inspect and remove worktrees; fetch branches; read PRs/checks/diffs; close PRs of cancelled tasks with a cancellation comment; update branches, retarget dependent PRs to the verified default branch and merge accepted PRs. Closing, merging, retargeting or marking a PR ready first checks that the PR GitHub answers at that address is the one Alondra recorded (its `id` and `fullDatabaseId`, read with `gh pr view --json`); when a read of a PR fails (the watcher's, or the merger's read before it merges), the same address goes to `gh api graphql` once to learn whether GitHub has nothing there or its API limit is spent; when a recorded PR's address stops answering (a moved or renamed repository), Alondra looks it up by its GitHub node id with `gh api graphql`. A merge the user authorized reads the base's rulesets (`gh api .../rulesets/{id}`) and, when every required check still running is one the signed-in account may bypass, merges with `gh pr merge --admin`. For each PR waiting to merge it also reads GitHub's verdict (`gh pr view` with `reviewDecision`, the required checks and the bypass list) to show why a merge waits, such as a review GitHub requires. Git may invoke configured credential helpers, SSH and repository hooks. | [Worktree launch](../crates/system/src/launch_worktree.rs), [worktree cleanup](../crates/system/src/worktree.rs), [GitHub operations](../crates/github-gh/src/cli.rs), [merges](../crates/github-gh/src/merges.rs), [merge verdict](../crates/github-gh/src/verdict.rs) |
| `brew` | `alondra update` invokes Homebrew and checks the new binaries. The daemon can run `brew upgrade` when `auto_update` is enabled (once per version, and three more times after a failure, recorded in `auto-update.json`, written through `auto-update.json.tmp` and renamed); it defaults to off. Before a new daemon binary replaces the running one, `alondrad --preflight` copies the domain databases and the state files startup reads (`domain-capabilities.json`, `devices.json`, `options.toml`, `options.pending`, `orchestrator.json`, `admission.json`) into a private temporary folder, starts there, and deletes the folder; it never writes the real HOME. | [update command](../crates/cli/src/update.rs), [automatic updates](../crates/daemon-server/src/updates.rs), [preflight](../crates/daemon-server/src/preflight.rs) |
| `git` and the suites of a repository with specs | Only when you or an agent run `alondra spec` inside a repository; it never contacts the daemon. Every subcommand runs `git rev-parse` and `git ls-files` there. `alondra spec run <component>` also starts each command that component's `boundary.toml` declares as a suite: without a shell, from the root of the repository, in a process group of its own that is stopped at the suite's time limit or when the command is interrupted. `validate`, `list` and `packet` start nothing else. | [Spec command](../crates/cli/src/spec.rs), [formats](architecture/specs.md) |

Setup and diagnostics also invoke macOS tools such as `launchctl` (manage the job), `plutil`
(read plists), `lsof` (locate a daemon), `scutil` (Mac name), `defaults` (language) and `open`
(browser or settings). The background-item status comes from ServiceManagement
(`SMAppService.statusForLegacyPlist`), which asks for no authorization. Upload processing can
run `sips` to convert an image. An explicitly configured Seatbelt confinement profile runs hosts
or probes through `sandbox-exec`; normal agent execution has no such confinement by default.
See [doctor](../crates/cli/src/doctor/system.rs), [uploads](../crates/engine-ports/src/uploads.rs),
[language](../crates/client-port/src/i18n.rs) and [backend configuration](../crates/engine-kernel/src/sdk_backend.rs).

Building from source additionally runs Bazel and its downloaded Rust, Swift and Node toolchains,
and the two programs `bazel/env/tools.bzl` pins: `simpool` and `musts`, downloaded over HTTPS from
their GitHub releases, checked against a SHA-256 and kept in Bazel's output base. The snapshot tests
run that `simpool` to lease iOS simulators from the pool in `~/Library/Developer/SimPool`. The
TriageTouch UI test instead creates one simulator of its own per run with `simctl` (a name unique to
the run, under `~/Library/Developer/CoreSimulator`) and deletes it when the run ends. The first
Bazel command also writes two launcher scripts, `musts` and `simpool`, into `~/.local/bin` (never over
a file it did not write) so both work in any shell; see [bazel/env/README.md](../bazel/env/README.md).
Rustc and clippy keep about 7 GB of incremental state for this repository's libraries and binaries in
`rustc-incremental*` inside Bazel's output base, which `bazelisk clean --expunge` deletes; a release
compiles from scratch. `make install` copies the three executables into `~/.local/bin` unless overridden; it is distinct
from the Homebrew installation. See [Makefile](../Makefile) and [source installation](building-from-source.md).

## Files read and written

The following are state families, including their temporary, lock, SQLite WAL/SHM and recovery
files. They can contain private work. Removing a package does not itself erase these files or
provider-owned sessions and credentials.

| Location | Contents and access | Source |
|---|---|---|
| `~/.alondra/config.toml`, `options.toml` | Read/write daemon settings, projects folder, optional worktree folder and its pinned volume identity, provider/model choices and private-name rules. Environment variables can override configuration. | [Config](../crates/core/src/config.rs), [options](../crates/core/src/options.rs) |
| `~/.alondra/domain.db` | Read/write SQLite task/card/session state, recorded worktree volume identities and committed read index. Migration can inspect old `alondra.db`, `schedules.db`, `domain-v2.db` and preserve retired state in `domain-retired-*`. | [Database startup](../crates/alondrad/src/lib.rs), [store](../crates/store-sqlite/src/connection.rs), [legacy inspection](../crates/daemon-server/src/legacy_home.rs) |
| `~/.alondra/domain-capabilities.json` | Read/write (0600) SHA-256 digests of the bearer credentials Alondra hands each node and orchestrator batch through the child environment; the bearers themselves are never written. A node's credential is retired when its host ends (closed, paused, lost or failed to start), and opening the daemon drops those of nodes whose host can no longer run. A full registry shows one card and holds new starts until a credential is retired. | [Registry](../crates/engine-kernel/src/domain_runtime/capabilities.rs), [authority](../crates/daemon-support/src/domain_authority.rs), [host ends](../crates/engine-sdk/src/sdk_owner.rs) |
| `~/.alondra/service.toml`, `service-keys/{signing.key,agreement.key,public.json,iroh.key}` | Device service identity, private signing/agreement keys, a separate iroh endpoint key, relay and device configuration. Key directory is 0700; private key files are 0600. Identity recovery keeps backups and a recovery marker. | [Enrollment](../crates/cli/src/service_setup.rs), [endpoint key](../crates/daemon-server/src/iroh_endpoint.rs), [recovery](../crates/cli/src/service_setup/recovery.rs) |
| `~/.alondra/tui-device`, `tui-key.pem`, `tui-pairing.lock`; `$TMPDIR/alondra-tui-restore-<pid>.json` | The terminal's own device ID and private signing key (0600), paired with this Mac's daemon over loopback on first use under the lock. When the daemon answers «wrong signature» or «device not paired» the terminal pairs again once per process, replacing both files, and says so once. The daemon reads `tui-device` to keep that device out of the device list, out of the sweep of unused devices and from being unpaired. A TUI that restarts on a new binary writes the restore file (0600) with the open screen, the text being written and the answers no Mac has confirmed yet (the request each one sends, which can hold text the user typed); the new process reads it once and deletes it. | [Local pairing](../crates/client-transport/src/local.rs), [restore](../crates/cli-tui/src/tui/restore.rs), [device list](../crates/daemon-server/src/domain_bootstrap.rs) |
| `~/.alondra/devices.json`, `direct-devices.json`, `revoked-devices.json`, `device-usage.json`, `iroh-terminals.json`, `service-receipts.sqlite`, `service-upload-attempts.sqlite` | Paired-device approvals/revocations, usage timestamps, endpoint proofs, replay-safe request receipts and resumable uploads. | [Device auth](../crates/daemon-server/src/auth.rs), [direct pairing](../crates/daemon-server/src/direct.rs), [revocations](../crates/daemon-server/src/revoked_devices.rs), [usage](../crates/daemon-server/src/device_usage.rs), [endpoints](../crates/daemon-server/src/iroh_terminals.rs), [service](../crates/daemon-server/src/service.rs), [uploads](../crates/daemon-server/src/service_files.rs) |
| `~/.alondra/sdk/`, `~/.alondra/codex/`, `~/.alondra/antigravity/` | Per-execution private host lock/bootstrap, socket `s`, `outbox` journal and recovery records; Codex account/thread identity records and native-thread mappings; the ACP session each Antigravity node runs in (`native-sessions/<session>`, a `0600` file with the session id, account, access and `agy` version, never a credential) and a private `TMPDIR` of each Antigravity host for its `agy`, removed when the host ends. Recovery also reads completed `.ended-*` host archives, and the `outbox` of a host that ended in its own folder (as one a move to another account ends), to retain definitive receipt proofs in `domain.db`, without connecting to those hosts or changing their files. The archive of a host closed while its node rested is deleted when a host that resumes the session takes its place, once everything it recorded is in `domain.db` and no command waits for its receipt. When the daemon starts it also deletes the `.ended-*` archives of tasks that are finished or cancelled, and all but the last one of an open task that owes no input, so none outlives the need for its receipts. A node's folder is deleted when its host ends and no open task claims it (paused tasks count as open), and when the task that claimed it closes or is cancelled; it stays while a host holds its lock, while a launch is preparing it, and while a command of a task that claimed it still waits for a receipt only that host's `outbox` can settle. An orchestrator batch deletes its folder when its host ends, unless the batch could not record in `domain.db` what the host observed. When the daemon starts it deletes the folders that already meet those conditions, and the batch folders no host holds. Before deleting a node's folder it keeps the identity of the account its session ran on under `native-identity/`, where reopening the task reads it. Backend JSON overrides can select different state roots, executables or confinement. | [Backend](../crates/engine-kernel/src/sdk_backend.rs), [ownership](../crates/agent-runtime/src/owned_host.rs), [host port and archive move](../crates/engine-ports/src/agent_host.rs), [journal](../agent-host/journal.ts), [archived receipts](../crates/agent-protocol/src/archived_receipts.rs), [ended-host sweep](../crates/engine-sdk/src/ended_hosts.rs), [host-folder sweep](../crates/engine-sdk/src/host_folders.rs), [Codex mapping](../agent-host/codex-session.ts) |
| `~/.alondra/orchestrator.json`, `~/.alondra/runtime/orchestrator/` | Read/write orchestrator state: its last runs and the conversation its batches resume (the provider session or thread id, the account it ran on, its size and the time of its last request), so every batch after the first resumes that conversation instead of starting one. `runtime/orchestrator/` is the orchestrator's working folder: `executor.lock`, `owner.json` with the process that runs a batch, and, when a conversation is replaced (it grew too large, its prompt cache expired while it was large, the batch runs on another account or provider, or it could not be resumed), `conversations/conversation-<session id>.md` (folder 0700, file 0600): Markdown condensed from Claude Code's own transcript of the old conversation, with no model call, which the new conversation can read; only the newest 20 are kept. | [Conversation](../crates/engine-orchestrator/src/orchestrator_conversation.rs), [batches](../crates/engine-orchestrator/src/domain_orchestrator.rs), [process ownership](../crates/engine-orchestrator/src/orchestrator_process.rs) |
| `<host directory>/wait.json` | Optional wait text written atomically by `alondra report-wait` or an agent command. The daemon reads at most 4 KiB, checks an optional process ID with signal 0, and deletes the report when the node's turn ends. Clearing or replacing it also ends the old report. It changes only what the task row shows. | [Format](wait-report.md), [writer](../crates/cli/src/report_wait.rs), [reader](../crates/engine-live/src/sdk_live.rs) |
| `~/.alondra/files/` | Documents, attachments, uploads and content copies for cards. The daemon reads requested worktree documents to deliver them here, and copies each file a node attaches to a card from the task's worktree into `files/domain-v2/<sha256>` when the card is presented, so the card keeps its own copy after the worktree changes or goes; on start it does the same for older cards whose files still match. A task's scratch folder `files/tasks/<task id>/` is removed when the task closes. When a large conversation gets an input after its prompt cache expired, whether its host was relaunched, adopted after a restart or kept alive through a pause, that host is ended and a new session takes the conversation over instead (the size and expiry come from `context-readings.json`, or, for a task with no reading kept there or a kept reading of 0 tokens, from the last answer in Claude Code's transcript of the session, read locally), and Alondra writes there the task's complete records as `task-records.md` and its approved plan as `approved-plan.md`, both for the new session to search on demand, and the conversation before it, as `conversation-<session id>.md`: Markdown condensed from Claude Code's own transcript of that session, with no model call, which the new session gets permission to read and nothing sends anywhere; `alondra files sweep` lists what the retention rule removes, and the daemon's hourly retention pass deletes exactly that: a saved result 14 days after its task closed and its card was seen, and what cards showed (uploads, images, copies of attachments, documents) 30 days after their card and task ended, or anything unowned older than 30 days. | [Content](../crates/daemon-support/src/domain_content.rs), [retention rule](../crates/engine-ports/src/files_retention.rs), [hourly pass](../crates/engine-kernel/src/retention.rs), [conversation file](../crates/engine-sdk/src/sdk_owner/session_refresh.rs), [scoped reads](../crates/daemon-server/src/scoped_reads.rs), [uploads](../crates/engine-ports/src/uploads.rs) |
| `~/.alondra/reports/`, `alondrad.log`, `alondrad.log.1`, `mcp-calls.log`, `mcp-calls.log.1` | Diagnostic report drafts/sending records and rotating daemon logs. Logs rotate at 10 MB, keeping one previous file. Each node's MCP server appends every command it sends, its answer or lost answer and how long it took to `mcp-calls.log`, which rotates at 2 MB keeping one previous file. | [Reports](../crates/cli-report/src/report/mod.rs), [retry](../crates/engine-ports/src/reports.rs), [logs](../crates/foundation/src/daemon_log.rs), [MCP calls](../crates/cli-mcp/src/mcp/calls_log.rs) |
| Other `~/.alondra` state | `hooks.sock` for local plugin events; `service-home.lock` and approval/operation locks; `daemon.lock`, held exclusively by the one daemon that writes the domain database (a second `alondrad` on the same HOME is refused before it opens it); `doctor.json`, `startup-failures.json`, admission/launch records, `context-readings.json` (each task's last measured conversation size and prompt-cache expiry, read back after a restart; an answer that used no tokens, as one for a failed request, does not change it), `brew-keg-path`, `auto-update.json` and provider catalogs. Optional legacy state remains readable for migration. `closed-task-nodes.json`, where older daemons kept the node of each released task, is deleted when the daemon starts: that record now lives in the domain database, written with the release. | [Hook socket](../crates/system/src/hook_socket.rs), [released tasks](../crates/daemon-engine/src/live.rs), [home lock](../crates/core/src/home_lock.rs), [startup failures](../crates/host/src/startup.rs), [saved records](../crates/foundation/src/save.rs), [doctor](../crates/cli/src/doctor/system.rs), [setup](../crates/cli/src/setup.rs) |
| Your projects folder and repositories | Enumerate projects and inspect Git metadata, status and changed files. Create task checkouts under `<repo>/.claude/worktrees/` by default, or `<worktrees_root>/<repo>/<task>` when configured; keep repository ownership and volume identity in `.alondra-repository.json` beside the external task folders, and use a sibling `.<task>.removing` folder while deleting them on the same volume; read `.worktreeinclude` and copy listed ignored files. Cleanup removes owned worktrees after delivery, and with an owned worktree the local branch `worktree-<name>` Claude Code made for it (`git branch -D`) when the branch is in the default branch or, once that task's pull request merged, its tip is the tip of the branch's own copy on a remote (or an ancestor of it), read from the remote with `git fetch` at that moment; a branch with any commit that exists in neither is never deleted, whatever its pull request did. That fetch contacts the remote with no prompt. An answered «delete the unsaved changes?» card of a cancelled task deletes its worktree once, only while the task is still cancelled and its worktree still kept for the user, and records it as removed, also a launch folder the task never recorded: a folder made at that path later, or a reopened task's worktree, is never covered by it, and cancelling it again asks anew. The startup sweep of cancelled tasks never touches a worktree handed to another task, one any task's record says the user kept («Leave it open») or is the user's own, one whose record already ended, one with commits no remote has, nor one another task still holds, even while that task closes. A worktree of `.claude/worktrees` that is a link to a folder elsewhere, such as another volume, goes with that folder, but only when the folder is a linked worktree of the same repository; while it cannot be reached, nothing of it is touched. What an ended task left where its worktree was, once Git removed the worktree, is deleted when the task closes and when the daemon starts: a link to nothing, or a folder with no file in it (with the link that points to it); a folder with anything in it, or one a task still open works in, is never touched. Making a worktree forgets Git's record of only the worktree that was in that folder, never the records of the repository's other worktrees. Only a folder Git lists as a worktree is recorded as a task's, never your home or the projects folder, so neither is ever inspected or deleted as one. Agent commands can change repository files and run builds. When a node asks permission to delete or edit files, the daemon reads the canonical folder and file metadata of each path it names, and for `git checkout`/`git restore` runs `git rev-parse` in that folder (nothing is written), to allow, without a card, what only changes files of the task's own linked worktrees. A new session that takes a conversation over also runs `git rev-parse --abbrev-ref HEAD` in the task's worktree (nothing is written) to tell it its branch. | [Project discovery](../crates/core/src/options.rs), [launch](../crates/system/src/launch_worktree.rs), [cleanup](../crates/system/src/worktree.rs), [own-worktree permissions](../crates/core/src/own_worktree.rs) |
| Requested secret destination, e.g. `<repo>/.env` | A secret-input answer writes the named variable into a Git-ignored file in both the task worktree and main checkout. The node receives a receipt rather than the secret value. | [Secret writes](../crates/system/src/secret_files.rs), [path validation](../crates/core/src/secret_destination.rs) |
| Claude profile (`~/.claude`, or configured profile), `~/.claude.json` | Read provider status, settings, plugins, MCP configuration, sessions, background-job/workflow records and trust. When a task closes or is cancelled, read the notes Claude Code keeps of the isolated subagents of the task's own sessions (`projects/*/<session>/subagents/agent-*.meta.json`, links not followed) to find worktrees they made that the task must settle. Read the transcript Claude Code keeps of the orchestrator's conversation (`projects/*/<session>.jsonl`, only once a record in it proves it ran in `~/.alondra/runtime/orchestrator`), to tell whether it can still be resumed and to condense it when it is replaced. Alondra can add trust entries for task repositories and remove user-added MCP configuration when requested. Added Claude accounts live under `~/.alondra/accounts/` with links to shared settings, skills and session history. | [Accounts](../crates/system/src/claude_account.rs), [trust](../crates/daemon-engine-model/src/claude_trust.rs), [MCP removal](../crates/engine-mcp/src/mcp_remove.rs), [job records](../crates/engine-ports/src/job_state.rs), [workflows](../crates/daemon-server/src/workflow_state.rs), [subagent worktree notes](../crates/provider-claude/src/session_worktrees.rs), [session transcripts](../crates/engine-ports/src/claude_history.rs) |
| Codex profile (`CODEX_HOME` or native default), `~/.alondra/provider-accounts/codex/` | Native settings, auth and session history; managed accounts use file-based credential storage. Alondra never signs in to Codex: it reads the session you started with `codex login` and removes what an older version left of an unfinished sign-in. | [Codex profiles](../crates/core/src/codex_account.rs), [sign-in](../crates/core/src/codex_login.rs), [credential publication](../crates/system/src/codex_credentials.rs) |
| `~/.gemini`, `~/.alondra/provider-accounts/antigravity/` | The default Antigravity account is `agy`'s own: `~/.gemini` and the sign-in `agy` keeps. Alondra never writes to either; `agy` itself records each check's empty session in its history, as it does any session. A named account gets `provider-accounts/antigravity/<id>`, a `0700` folder that is never a link, holding links to your `~/.gemini` rules, skills, commands, extensions, hooks, settings and MCP servers (never a sign-in or a conversation), the Google sign-in `agy` keeps there (`GEMINI_HOME` and `--gemini-dir` name the folder, with `AGY_ACP_FORCE_FILE_STORAGE=1` and a loopback `SSH_CONNECTION` so it lands in a file), and its key file `gemini-api-key`. Removing the account deletes the folder and never what its links point to. Account checks run in `provider-accounts/antigravity-probes/<random>`, removed after each check. | [folders](../crates/system/src/antigravity_account.rs), [shared names](../crates/core/src/antigravity_account.rs) |
| Mac app Application Support and preferences | `Application Support/com.bitomule.alondra.mac/CommandJournal` holds per-binding `operations.sqlite`, initialization markers, `connection-measurements.jsonl` and `loopback-identity/{signing-key,binding-secret,device-id}`. UserDefaults holds preferences and local notification receipts. A sandboxed app resolves Application Support inside its container. | [Root](../apple/Data/Account/Sources/Composition/AppStorageLocation.swift), [Mac composition](../apple/macOS/ViewsShipping/MacAppDelegate.swift), [loopback identity](../apple/Data/Transport/Sources/Loopback/LoopbackIdentityStore.swift), [journal](../apple/Data/Transport/Sources/Journal/EnvelopeJournal.swift), [notices](../apple/Data/Notifications/Sources/LocalNotices/LocalNoticeLedger+UserDefaults.swift) |
| `<repo>/alondra.toml` and the folders it names | `alondra spec` reads the settings, the specs, decisions and reference prose they point at, and the files of the repository git lists, to fingerprint them; a file that leaves the checkout through a link is not read. `run` and `record` write ledger entries under `<specs folder>/ledger/` in that repository and nowhere else, never through a link. `record --evidence <file>` reads the files you name to fingerprint them; it copies nothing. | [Spec command](../crates/cli/src/spec.rs), [formats](architecture/specs.md) |
| A task's worktree in a repository with specs | The daemon reads the task's committed files from git's objects (`git rev-parse`, `git ls-tree`, `git cat-file`, and `git merge-base` with the remote's default branch) when its author proposes specs, when you edit them on the plan's card, when a lasting decision is proposed and at each final delivery of a task in such a repository; it reads `alondra.toml` from the worktree's root while a plan waits for you or the task has specs. Once you approve them, it writes the approved spec, boundary and decision files into the task's own worktree, only under the specs and decisions folders and `alondra.toml`, never through a link and only while no agent of the task works; nothing is committed or pushed by it. | [Spec files of a worktree](../crates/system/src/spec_worktree.rs), [checks](../crates/daemon-server/src/spec_checks.rs), [writes](../crates/engine-sdk/src/spec_writes.rs), [formats](architecture/specs.md) |

The CLI's service login also uses a user-wide
`~/Library/Caches/<legacy app namespace>/service-login.lock`; this lock follows the OS account's
home even when HOME is overridden. See [account login](../crates/cli/src/account.rs).

The daemon also reads macOS process and resource information: process ancestry/liveness through
libproc/sysctl, memory pressure and swap counters, and free space of the startup disk, and once at
each launch of the folder the task's worktree goes in. It keeps admission/incident measurements under Alondra's home and raises
its own open-file soft limit when needed. These probes do not require walking the disk's files.
See [the process table and its signals](../crates/system/src/lib.rs),
[what a paused node still runs](../crates/engine-admission/src/process_watch.rs),
[machine measurements](../crates/system/src/machine.rs),
[disk measurements](../crates/system/src/disk.rs),
[incident records](../crates/engine-admission/src/machine_watch.rs) and
[file limit](../crates/daemon-server/src/open_files.rs).

## Credentials and authority

- **Claude login:** Claude Code owns its login and Keychain credentials. Alondra invokes
  `claude auth`, reads status/account metadata and selects profiles; it does not extract Claude
  tokens. See [Claude accounts](../crates/system/src/claude_account.rs).
- **Codex login:** Alondra neither starts a Codex sign-in nor stores Codex credentials; you sign
  in with `codex login` in Terminal. It only reads the selected profile's session status and, on
  start, removes the staging folder or half-published credential an older version left behind.
  Treat these folders as credential access. See
  [credential cleanup](../crates/system/src/codex_credentials.rs).
- **Antigravity sign-in:** Alondra never signs in to Google. A Google or Enterprise account is
  signed in with `agy acp --login` in Terminal; a named one with the command its Accounts row
  shows, which names its folder. A Cloud project account signs in with
  `gcloud auth application-default login --project <project>`, or uses a key; Alondra keeps only
  the path of a service-account key file you name with `--credentials-file`, never its contents.
- **Antigravity API keys:** the only credential Alondra keeps. A Gemini API key (or an Agent
  Platform key of a Cloud project account) is entered in the account's settings in the app or
  with `alondra accounts set-key antigravity/<account>`, which reads it without echo (or from a
  pipe, and never prompts under `--no-input`). It reaches the daemon inside the account's
  credential action: from an app over its own connection to the daemon, and from the CLI as a
  request signed with the user's local identity to the daemon's local port, which travels in plain
  HTTP inside the Mac and is not encrypted. The daemon writes it to
  `~/.alondra/provider-accounts/antigravity/<account>/gemini-api-key`: a `0600` file written whole
  to a new file of the folder, synced, then renamed over the old one, and read only when it is
  a regular file of the folder's owner, no one else can read, with no other link. Only that account's `agy` gets it, as `GEMINI_API_KEY`
  (or `GOOGLE_API_KEY`) in its environment at start, and so do the hooks and MCP servers that
  `agy` starts from the settings the folder links to in your `~/.gemini`. It is never in `options.toml`, `domain.db`, a
  log line, an action's reply or any screen: the domain records only that the account's credential
  changed, and clients see "Key set" or "No key". `--clear` (or Remove key) deletes the file;
  removing the account deletes its folder and the key with it. The default account takes no key.
  A node on the account gets only the file's path in its settings: its Antigravity host reads the
  file when it starts that node's `agy`, so the key is never in the host's `host.lock`, journal or
  observations, nor in any other node's environment.
  Treat the folder as credential access. See [key file](../crates/system/src/antigravity_account.rs)
  and [key action](../crates/daemon-engine/src/accounts.rs).
- **An API key entered in the app:** the account's settings on the Mac and the iPhone have a key
  section (*Gemini API key*, or *Agent Platform key* for a Cloud project account) with "Key set" or
  "No key", a secure field that always starts empty, and Remove key when one is set. Saving sends
  the key once, as the text of the account's credential action, and empties the field at once;
  the section then shows the daemon's answer and the account check that ran after it. Like every
  answer from the app, the action waits in the app's pending answers, encrypted with that device's
  key, until the daemon confirms it, and is sealed end to end for the Mac on the way, so the relay
  never reads it. Once confirmed the device keeps only a digest of the whole request, never the
  key. The app never stores, logs or shows the key back. See
  [key section](../apple/Features/Sources/Settings/AccountRowView.swift) and
  [pending answers](../apple/Data/Feed/Sources/ActionIntentDatabase.swift).
- **Alondra Cloud login:** The CLI stores its refresh credential in Keychain; the daemon does
  not read that item. The Mac app uses `com.bitomule.alondra.mac.auth` for refresh tokens and
  `com.bitomule.alondra.mac.service-pairing.v1` for pairing secrets. The app's token item uses
  `AfterFirstUnlockThisDeviceOnly`, pairing uses `WhenUnlockedThisDeviceOnly`.
  See [CLI account](../crates/cli/src/account.rs), [Mac app](../apple/macOS/ViewsShipping/MacAppDelegate.swift),
  [token storage](../apple/Data/Account/Sources/Auth/KeychainRefreshTokenStorage.swift) and
  [pairing store](../apple/Data/Pairing/Sources/PairingStore.swift).
- **Device keys:** Service signing/agreement keys and local loopback identity keys authenticate
  and encrypt device traffic. These are separate from provider logins. Paired-device approval
  is local to the Mac. See [enrollment](../crates/cli/src/service_setup.rs).
- **GitHub/Git:** `gh` and Git use their own configured credentials and helpers, including any
  SSH agent socket. Alondra invokes them with authority to read, close and merge PRs and publish
  diagnostic reports. See [GitHub runner](../crates/github-gh/src/cli.rs).
- **Agent environment and integrations:** Hosts pass a selected environment (including proxy,
  provider endpoint, GitHub and SSH settings), not the entire daemon environment.
  Claude and Codex receive `ALONDRA_WAIT_FILE`, the path to their own host's `wait.json`;
  commands can inherit it to report a wait. Without it the reporting command does nothing.
  `node_environment` can add variables; their values may persist in host ownership records.
  Claude nodes get `CLAUDE_CODE_PROMPT_CACHE_TTL=1h` and `CLAUDE_CODE_SUBAGENT_PROMPT_CACHE_TTL=5m`,
  which set how long Anthropic keeps their prompt cache.
  The orchestrator's Claude batches get `CLAUDE_CODE_PROMPT_CACHE_TTL=1h` too, since each batch resumes
  the same conversation.
  MCP servers and provider plugins have their own credentials and access. See
  [environment selection](../crates/engine-kernel/src/sdk_backend.rs).
- **Bounded diagnostics without restarting:** On this Mac, run
  `alondra observe --seconds 900 --delay 120` to start request and view counters on the running
  daemon. Duration is 1–3600 seconds (default 900); delay is 0–3600 seconds (default 0).
  Start/end records appear in the existing daemon log. Counting stops automatically and another
  command can start a fresh window afterward. Overlapping windows are refused, including a
  pending delay or an observation enabled at startup. The command changes no settings or files
  and never restarts the daemon. Its local JSON endpoint refuses paired devices, non-loopback
  peers, proxy headers, browser origins and non-loopback hosts. The command uses this Mac even
  when the TUI has selected a remote Mac. `--json` prints the accepted duration and delay.
- **Optional view diagnostics:** For observation at startup, the daemon reads `ALONDRA_VIEW_OBSERVE_SECONDS` once when
  opening its client. An integer from 1 to 3600 enables one aggregate observation window;
  `ALONDRA_VIEW_OBSERVE_DELAY_SECONDS` delays its start by 0 to 3600 seconds (default 60).
  Missing duration or invalid values disable it. The window writes two records to the existing
  daemon log with fixed counter names and numeric totals for executed view work, serialization
  bytes and invalidation causes. Byte totals distinguish nodes, pull requests, activity, task
  history, task maps and other values. Serializations count each component actually encoded;
  unchanged components reuse their hashes. JSON structure bytes stay zero for component hashes.
  Additional aggregate counters expose bytes hashed, hash operations, component comparisons,
  outer rows compared, materialized snapshots and copied node rows. They do not log the values.
  Visited agents and emitted nodes are counted separately. Feed counters distinguish changed replies, timeout replies
  (including zero-wait requests), errors and requests superseded by another feed for the same
  device. Superseded counts use the declared view client (`tui`, `ios`, `mac`), not a verified
  application identity. Concurrent TUI requests sharing a device keep their existing connections
  waiting for changes, with their original long-poll deadlines; another terminal does not make
  them return a superseded response and immediately send a replacement. Each pending terminal's
  views remain included in live sampling until its request ends. Diagnostics record no view keys,
  device IDs, paths or request contents, create no additional file, and stop counting
  automatically. Counts near a
  window boundary are approximate under concurrency; they are not CPU measurements. Variables
  must be present in the daemon's own startup environment: exporting them in another terminal
  does not affect a running daemon. Setup does not add them to the launchd job or provider
  environment. See [view observation](../crates/daemon-server/src/views/observation.rs).
- **Optional HTTP authentication diagnostics:** `ALONDRA_AUTH_OBSERVE_SECONDS` (1–3600)
  enables one aggregate window when the authentication state opens inside the daemon's Tokio
  runtime. `ALONDRA_AUTH_OBSERVE_DELAY_SECONDS` (0–3600, default 60) delays it. Missing or invalid
  settings disable it; opening outside a runtime also disables it. The existing daemon log
  receives start/end records containing fixed counter names and numbers, with actual elapsed
  microseconds for calculating rates. No additional file is created. Counters distinguish
  requests, missing signature headers, local unsigned passes, body-read rejections, verification
  attempts, key decoding/preparation, executed ECDSA and verification results. Verification
  success does not imply the handler's later durable authorization succeeded. Replayed requests
  can still execute ECDSA and are counted as rejected. Routes use a fixed catalog; `client` in
  view/feed queries and the presence of an MCP-operation header are only declared origins,
  never application identities. Unknown or ambiguous declarations stay unknown. No device IDs,
  names, addresses, operation IDs, query values, signatures or request bodies are logged. The
  scope is the main HTTP listener, not direct/iroh envelopes, pairing or outgoing signatures.
  Boundary events can race the window, so stage totals need not balance exactly. Counters are
  not CPU measurements. These variables must be set in the daemon's startup environment; they
  are not installed into launchd or provider environments. See
  [HTTP authentication observation](../crates/daemon-server/src/auth/observation.rs).
- **Optional direct APNs:** `~/.alondra/push.json` holds notification registration state;
  `~/.alondra/apns.json` names an Apple `.p8` private key file, team,
  key ID and topic. The daemon reads that key to send notifications when direct APNs is
  configured. See [push](../crates/daemon-server/src/push.rs).
- **`BAZEL_TEST` (tests only):** `bazel test` sets it; setup never installs it into the launchd
  job or a provider environment. When present in the daemon's or CLI's own environment, the
  process behaves as under test: it reads an empty temporary Claude folder instead of
  `~/.claude`, skips the flush to disk after atomic writes, relaunches a lost host without
  waiting, shortens the message acknowledgement and «Allow always» settings waits, and checks
  each owner transaction against a reload of the database. See
  [under test](../crates/foundation/src/lib.rs) and
  [Claude folder](../crates/engine-ports/src/watcher.rs).

## Connections

| Connection | Default and data involved | Source |
|---|---|---|
| Local CLI/domain API | HTTP on `127.0.0.1:47800` by default; configurable address/port. Local task/card commands and reads. | [Configuration](../crates/core/src/config.rs), [API](../crates/daemon-server/src/domain_api.rs) |
| Device HTTP transport | TCP `47810`, all IPv4 interfaces by default, configurable. Service requests/responses are authenticated encrypted envelopes, not a public task API. | [Service listener](../crates/daemon-server/src/service.rs) |
| iroh transport | UDP `47811` on IPv4/IPv6, with an available-port fallback; direct encrypted device streams and a relay learned from the service. Router mapping uses UPnP/NAT-PMP/PCP when available. | [Endpoint](../crates/daemon-server/src/iroh_endpoint.rs) |
| Local discovery and permission probe | Bonjour `_alondra._tcp` advertises/discovers the Mac; a one-byte UDP probe to the gateway on port 9 checks Local Network access. | [Bonjour](../crates/host/src/bonjour.rs), [probe](../crates/system/src/local_network.rs) |
| Alondra Cloud (optional enrollment) | HTTPS `https://api.getalondra.com` by default, configured in `service.toml`. Registration, device routing, heartbeat, push delivery and relay discovery. Device content is sealed; the control plane receives routing/public identity/push metadata. | [Service config](../crates/core/src/service_config.rs), [relay](../crates/daemon-server/src/service/relay.rs), [enrollment](../crates/cli/src/service_setup.rs) |
| Account sign-in | Browser and HTTPS OIDC at `https://auth.getalondra.com/realms/alondra`, using discovered authorization/token/userinfo/revocation endpoints. A temporary loopback callback receives the CLI browser sign-in. | [CLI login](../crates/cli/src/account.rs), [app OIDC](../apple/Data/Account/Sources/Auth/OIDCConfiguration.swift) |
| Providers and MCP | Claude Code/Codex contact their provider using the selected login and endpoint settings; MCP tools contact their configured services. Prompts, code and tool results can leave the Mac through these tools. | [Claude host](../agent-host/session.ts), [Codex host](../agent-host/codex-session.ts) |
| Antigravity | `agy` contacts Google with the account's sign-in, key or project when Alondra checks the account, reads its models and reads its usage, and when a node runs on the account: then prompts, code and tool results leave the Mac through `agy`, as they do with Claude Code and Codex. Alondra itself opens no connection to Google. | [account checks](../crates/system/src/antigravity_account.rs) |
| GitHub, Git remotes and PR images | `gh`/Git contact the configured host/remotes. Final deliveries reread their task's unmerged PRs before admission, including closed PRs that may have reopened; a successful node `gh pr close` schedules a fresh read too. The open-PR sweep schedules closure of registered PRs whose tasks are cancelled, once: a PR Alondra closed that someone reopens on GitHub is handed to the user as an open PR and never closed again; the cancellation comment sent to GitHub contains the task title. PR preview fetching can download GitHub image attachments, including signed `private-user-images.githubusercontent.com` and recognized `raw.githubusercontent.com` image URLs. | [GitHub](../crates/github-gh/src/cli.rs), [open-PR sweep](../crates/engine-github/src/open_pr_sweep.rs), [images](../crates/engine-github/src/pr_images.rs), [image downloads](../crates/github-gh/src/images.rs) |
| Release checks and updates | HTTPS GitHub API `api.github.com/repos/bitomule/homebrew-tap/releases?per_page=100`; override `ALONDRA_UPDATE_URL`. Homebrew downloads packages when an update is requested or automatic updates are enabled. | [Release URL](../crates/core/src/update.rs), [updater](../crates/daemon-server/src/updates.rs), [formula](../deploy/brew/alondra.rb.in) |
| Diagnostic reports (when sent) | Uses `gh api` to upload the previewed, sanitized bundle to a secret gist and create an issue linking it. Anyone with the issue's gist URL can read it. Report retrieval downloads from `gist.githubusercontent.com`. Destination comes from relay bootstrap or `report.repo`. | [Report preparation](../crates/cli-report/src/report/mod.rs), [sending](../crates/cli-report/src/report/send.rs), [GitHub transport](../crates/cli-report/src/report/github.rs) |
| Direct APNs (optional) | HTTPS `api.push.apple.com` or `api.sandbox.push.apple.com`, authenticated with the configured Apple key. Enrolled-service push normally goes through the relay instead. | [Push](../crates/daemon-server/src/push.rs) |

When a TUI view request receives no response, the CLI probes the same configured destination
with an empty change-feed request (`wait=0`) and a five-second availability decision deadline.
Any HTTP response confirms connectivity; the operation still uses the transport's normal
authentication and response validation. See [availability checks](../crates/client-port/src/client/availability.rs).

DNS, proxies, provider-specific endpoints and network connections made by Homebrew, Git,
repository commands or MCP servers depend on your configuration. This table is not a fixed
firewall policy. The [device protocol](iroh-protocol.md) describes encryption and pairing.

## macOS permissions

- **Background execution:** The LaunchAgent appears in Login Items & Extensions. Disabling it
  prevents launchd from keeping the daemon running. See [doctor's background-item check](../crates/cli/src/doctor/system.rs).
- **Local Network:** The daemon/launcher and Mac app use the local network for discovery and
  device transport. macOS can ask for permission. The launcher has a stable installed path to
  retain that permission across Homebrew upgrades. See [daemon plist](../crates/alondrad/Info.plist),
  [Mac app plist](../apple/macOS/Info.plist) and [launcher](../crates/host/src/launcher.rs).
- **Notifications:** The Mac app requests alert, sound and badge permission. See
  [notification center](../apple/Data/Notifications/Sources/LocalNotices/SystemLocalNoticeCenter.swift).
- **Files and Keychain:** The daemon/CLI operate as your user; they need access to state,
  provider profiles and the selected projects. Protected locations may trigger macOS file
  privacy prompts. Choose a projects folder outside protected Desktop/Documents/Downloads;
  Alondra checks that choice. Keychain access is as described above. See
  [folder validation](../crates/core/src/projects_folder.rs) and [Mac folder picker](../apple/macOS/ViewsShipping/LocalFolderPanel.swift).
- **App sandbox and executable signing:** The Mac App Store configuration enables the app
  sandbox, network client/server and user-selected read/write access. The bundled Node hosts
  carry the JIT signing entitlement. These are distinct from the permissions of agent commands.
  See [app entitlements](../apple/entitlements.bzl) and [host entitlement](../agent-host/host-entitlements.plist).

Alondra's own app does not request camera, microphone, screen recording, Accessibility or
Automation access in this implementation. An agent or external tool you run can have additional
requirements; granting those permissions is outside this inventory.

## Audit before installing

Give an agent a checkout of the release you intend to install and this prompt:

> Audit this checkout against `docs/on-your-mac.md` before I install Alondra. Read the actual
> code, install formula, launchd template, plugin hooks/MCP configuration, agent hosts, daemon,
> CLI and Mac app. Do not install, launch, update or sign in to anything. Trace every installed
> executable and child process, file read/write/delete, credential/Keychain operation, network
> listener and outbound connection, permission request and environment handoff. Distinguish
> defaults from opt-in features and Alondra's own operations from provider/agent/MCP behavior.
> Report omissions or contradictions with repository paths and line numbers, the affected data
> and who can receive it. Check README and INSTALL agree. Check the `mac-inventory-review`
> paths in MUSTS.yml cover every source of these effects, including any new source files.
> State what you could not verify; do not infer safety from tests passing or from this document.

For contributors, `mac-inventory-review` in [MUSTS.yml](../MUSTS.yml) requires a source review
when installation/runtime paths change. Update this inventory for changed effects; when the
inventory remains accurate, record why with source references. A separate Bazel policy test
checks that the review cannot disappear or lose coverage of the installation/runtime surfaces.
