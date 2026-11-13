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
| `alondra`, `alondrad`, `alondrad-launcher` | Homebrew installs these in `<prefix>/bin` through its Cellar/opt links. The CLI opens the TUI, performs setup and serves MCP. The daemon owns tasks and devices. The launcher starts the daemon. | [Formula](../deploy/brew/alondra.rb.in), [setup](../crates/alondra/src/setup.rs) |
| `~/.alondra/bin/alondrad-launcher` | Setup copies the bundled launcher to this stable path. launchd starts it at login and keeps the daemon running. The default job is `dev.alondra.alondrad`, in `~/Library/LaunchAgents/dev.alondra.alondrad.plist`; the label is configurable. The plist supplies PATH, HOME, Alondra home and selected provider profile paths, and sends stdout/stderr to the daemon log. | [Launcher](../crates/core/src/launcher.rs), [plist template](../launchd/alondra.plist.in) |
| `alondra-host`, `alondra-codex-host` | Bundled executables under `<prefix>/opt/alondra/libexec/alondra/{sdk,codex}` contain Node and Alondra's host code. Hosts run for active agent sessions, with private Unix sockets and journals; they can survive a daemon restart. | [Backend selection](../crates/daemon-engine/src/sdk_owner/backend.rs), [host ownership](../crates/agent-runtime/src/owned_host.rs) |
| Your `claude` or `codex` | The Claude Agent SDK host launches your installed Claude Code. The Codex host launches `codex app-server --listen stdio://`. Account/model/MCP probes also start these tools. Provider settings can add hooks, plugins and MCP subprocesses. | [Claude session](../agent-host/session.ts), [Codex session](../agent-host/codex-session.ts), [probes](../crates/daemon-engine/src/native_probe.rs) |
| Alondra node and orchestrator plugins | Homebrew ships the marketplace and plugins under `libexec/alondra`. The daemon maintains copies in `~/.alondra/plugin` and `~/.alondra/plugin-orchestrator`, including a wrapper pointing at its CLI. Claude setup registers the local marketplace and disables older globally enabled Alondra plugins; Alondra loads the relevant plugin for its own sessions. Node hooks call `alondra hook`; MCP starts `alondra mcp-node` or `alondra mcp-orchestrator`. | [Plugin installation](../crates/daemon-support/src/plugins.rs), [node hooks](../plugin/hooks/hooks.json), [node mod](../plugin/hooks/register.ts), [MCP](../plugin/.mcp.json), [orchestrator MCP](../plugin-orchestrator/.mcp.json) |
| `git`, `gh` | Create, inspect and remove worktrees; fetch branches; read PRs/checks/diffs; close PRs of cancelled tasks with a cancellation comment; update branches and merge accepted PRs. Git may invoke configured credential helpers, SSH and repository hooks. | [Worktree launch](../crates/daemon-support/src/launch_worktree.rs), [worktree cleanup](../crates/daemon-support/src/worktree.rs), [GitHub operations](../crates/daemon-engine/src/domain_github.rs) |
| `brew`, `tuicr` | Setup can install missing `tuicr`; the TUI runs it to review diffs. `alondra update` invokes Homebrew and checks the new binaries. The daemon can run `brew upgrade` when `auto_update` is enabled; it defaults to off. | [Setup](../crates/alondra/src/setup.rs), [PTY runner](../crates/alondra/src/pty.rs), [update command](../crates/alondra/src/update.rs), [automatic updates](../crates/daemon-support/src/updates.rs) |

Setup and diagnostics also invoke macOS tools such as `launchctl` (manage the job), `plutil`
(read plists), `lsof` (locate a daemon), `scutil` (Mac name), `defaults` (language) and `open`
(browser or settings). The background-item status comes from ServiceManagement
(`SMAppService.statusForLegacyPlist`), which asks for no authorization. Upload processing can
run `sips` to convert an image. An explicitly configured Seatbelt confinement profile runs hosts
or probes through `sandbox-exec`; normal agent execution has no such confinement by default.
See [doctor](../crates/alondra/src/doctor/system.rs), [uploads](../crates/daemon-support/src/uploads.rs),
[language](../crates/alondra/src/i18n.rs) and [backend configuration](../crates/daemon-engine/src/sdk_owner/backend.rs).

Building from source additionally runs Bazel and its downloaded Rust, Swift and Node toolchains.
`make install` copies the three executables into `~/.local/bin` unless overridden; it is distinct
from the Homebrew installation. See [Makefile](../Makefile) and [source installation](building-from-source.md).

## Files read and written

The following are state families, including their temporary, lock, SQLite WAL/SHM and recovery
files. They can contain private work. Removing a package does not itself erase these files or
provider-owned sessions and credentials.

| Location | Contents and access | Source |
|---|---|---|
| `~/.alondra/config.toml`, `options.toml` | Read/write daemon settings, projects folder, provider/model choices and private-name rules. Environment variables can override configuration. | [Config](../crates/core/src/config.rs), [options](../crates/core/src/options.rs) |
| `~/.alondra/domain.db` | Read/write SQLite task/card/session state and committed read index. Migration can inspect old `alondra.db`, `schedules.db`, `domain-v2.db` and preserve retired state in `domain-retired-*`. | [Database startup](../crates/alondrad/src/lib.rs), [store](../crates/daemon-engine/src/domain_store/connection.rs), [legacy inspection](../crates/daemon-support/src/domain_cutover.rs) |
| `~/.alondra/service.toml`, `service-keys/{signing.key,agreement.key,public.json,iroh.key}` | Device service identity, private signing/agreement keys, a separate iroh endpoint key, relay and device configuration. Key directory is 0700; private key files are 0600. Identity recovery keeps backups and a recovery marker. | [Enrollment](../crates/alondra/src/service_setup.rs), [endpoint key](../crates/alondrad/src/iroh_endpoint.rs), [recovery](../crates/alondra/src/service_setup/recovery.rs) |
| `~/.alondra/devices.json`, `direct-devices.json`, `revoked-devices.json`, `device-usage.json`, `iroh-terminals.json`, `service-receipts.sqlite`, `service-upload-attempts.sqlite` | Paired-device approvals/revocations, usage timestamps, endpoint proofs, replay-safe request receipts and resumable uploads. | [Device auth](../crates/daemon-support/src/auth.rs), [direct pairing](../crates/alondrad/src/direct.rs), [revocations](../crates/daemon-support/src/revoked_devices.rs), [usage](../crates/daemon-support/src/device_usage.rs), [endpoints](../crates/daemon-support/src/iroh_terminals.rs), [service](../crates/alondrad/src/service.rs), [uploads](../crates/daemon-support/src/service_files.rs) |
| `~/.alondra/sdk/`, `~/.alondra/codex/` | Per-execution private host lock/bootstrap, socket `s`, `outbox` journal and recovery records; Codex account/thread identity records and native-thread mappings. Backend JSON overrides can select different state roots, executables or confinement. | [Backend](../crates/daemon-engine/src/sdk_owner/backend.rs), [ownership](../crates/agent-runtime/src/owned_host.rs), [journal](../agent-host/journal.ts), [Codex mapping](../agent-host/codex-session.ts) |
| `<host directory>/wait.json` | Optional wait text written atomically by `alondra report-wait` or an agent command. The daemon reads at most 4 KiB, checks an optional process ID with signal 0, and deletes the report when the node's turn ends. Clearing or replacing it also ends the old report. It changes only what the task row shows. | [Format](wait-report.md), [writer](../crates/alondra/src/report_wait.rs), [reader](../crates/daemon-engine/src/sdk_live.rs) |
| `~/.alondra/files/` | Documents, attachments, uploads and content copies for cards. The daemon reads requested worktree documents to deliver them here, and copies each file a node attaches to a card from the task's worktree into `files/domain-v2/<sha256>` when the card is presented, so the card keeps its own copy after the worktree changes or goes; on start it does the same for older cards whose files still match. | [Content](../crates/daemon-support/src/domain_content.rs), [scoped reads](../crates/daemon-support/src/scoped_reads.rs), [uploads](../crates/daemon-support/src/uploads.rs) |
| `~/.alondra/reports/`, `alondrad.log`, `alondrad.log.1`, `mcp-calls.log`, `mcp-calls.log.1` | Diagnostic report drafts/sending records and rotating daemon logs. Logs rotate at 10 MB, keeping one previous file. Each node's MCP server appends every command it sends, its answer or lost answer and how long it took to `mcp-calls.log`, which rotates at 2 MB keeping one previous file. | [Reports](../crates/alondra/src/report/mod.rs), [retry](../crates/daemon-support/src/reports.rs), [logs](../crates/core/src/daemon_log.rs), [MCP calls](../crates/alondra/src/mcp/calls_log.rs) |
| Other `~/.alondra` state | `hooks.sock` for local plugin events; `service-home.lock` and approval/operation locks; `doctor.json`, `startup-failures.json`, admission/launch records, `brew-keg-path`, `auto-update.json` and provider catalogs. Optional legacy state remains readable for migration. | [Hook socket](../crates/daemon-support/src/hook_socket.rs), [home lock](../crates/core/src/home_lock.rs), [startup failures](../crates/core/src/startup.rs), [saved records](../crates/daemon-support/src/save.rs), [doctor](../crates/alondra/src/doctor/system.rs), [setup](../crates/alondra/src/setup.rs) |
| Your projects folder and repositories | Enumerate projects and inspect Git metadata, status and changed files. Create task checkouts under `<repo>/.claude/worktrees/`; read `.worktreeinclude` and copy listed ignored files. Cleanup removes owned worktrees after delivery. Only a folder Git lists as a worktree is recorded as a task's, never your home or the projects folder, so neither is ever inspected or deleted as one. Agent commands can change repository files and run builds. | [Project discovery](../crates/core/src/options.rs), [launch](../crates/daemon-support/src/launch_worktree.rs), [cleanup](../crates/daemon-support/src/worktree.rs) |
| Requested secret destination, e.g. `<repo>/.env` | A secret-input answer writes the named variable into a Git-ignored file in both the task worktree and main checkout. The node receives a receipt rather than the secret value. | [Secret writes](../crates/daemon-support/src/secret_files.rs), [path validation](../crates/core/src/secret_destination.rs) |
| Claude profile (`~/.claude`, or configured profile), `~/.claude.json` | Read provider status, settings, plugins, MCP configuration, sessions, background-job/workflow records and trust. Alondra can add trust entries for task repositories and remove user-added MCP configuration when requested. Added Claude accounts live under `~/.alondra/accounts/` with links to shared settings, skills and session history. | [Accounts](../crates/core/src/claude_account.rs), [trust](../crates/daemon-engine/src/claude_trust.rs), [MCP removal](../crates/daemon-engine/src/mcp_remove.rs), [job records](../crates/daemon-support/src/job_state.rs), [workflows](../crates/daemon-support/src/workflow_state.rs) |
| Codex profile (`CODEX_HOME` or native default), `~/.alondra/provider-accounts/codex/` | Native settings, auth and session history; managed accounts use file-based credential storage. Sign-in uses a staging profile before publishing credentials. | [Codex profiles](../crates/core/src/codex_account.rs), [sign-in](../crates/core/src/codex_login.rs), [credential publication](../crates/daemon-support/src/codex_credentials.rs) |
| Mac app Application Support and preferences | `Application Support/com.bitomule.alondra.mac/CommandJournal` holds per-binding `operations.sqlite`, initialization markers, `connection-measurements.jsonl` and `loopback-identity/{signing-key,binding-secret,device-id}`. UserDefaults holds preferences and local notification receipts. A sandboxed app resolves Application Support inside its container. | [Root](../apple/Data/Account/Sources/Composition/AppStorageLocation.swift), [Mac composition](../apple/macOS/ViewsShipping/MacAppDelegate.swift), [loopback identity](../apple/Data/Transport/Sources/Loopback/LoopbackIdentityStore.swift), [journal](../apple/Data/Transport/Sources/Journal/EnvelopeJournal.swift), [notices](../apple/Data/Notifications/Sources/LocalNotices/LocalNoticeLedger+UserDefaults.swift) |

The CLI's service login also uses a user-wide
`~/Library/Caches/<legacy app namespace>/service-login.lock`; this lock follows the OS account's
home even when HOME is overridden. See [account login](../crates/alondra/src/account.rs).

The daemon also reads macOS process and resource information: process ancestry/liveness through
libproc/sysctl, memory pressure and swap counters, and free space/mount metadata for the startup
disk and build volumes. It keeps admission/incident measurements under Alondra's home and raises
its own open-file soft limit when needed. These probes do not require walking the disk's files.
See [process watch](../crates/daemon-support/src/process_watch.rs),
[machine measurements](../crates/daemon-engine/src/machine.rs),
[disk measurements](../crates/daemon-support/src/disk.rs),
[incident records](../crates/daemon-engine/src/machine_watch.rs) and
[file limit](../crates/daemon-support/src/open_files.rs).

## Credentials and authority

- **Claude login:** Claude Code owns its login and Keychain credentials. Alondra invokes
  `claude auth`, reads status/account metadata and selects profiles; it does not extract Claude
  tokens. See [Claude accounts](../crates/core/src/claude_account.rs).
- **Codex login:** Alondra invokes native login and account APIs. During staged sign-in it
  reads and replaces the selected profile's `auth.json` and, when present, its `Codex Auth`
  Keychain item. Managed profiles use file storage. Treat these folders and the sign-in process
  as credential access. See [credential publication](../crates/daemon-support/src/codex_credentials.rs).
- **Alondra Cloud login:** The CLI stores its refresh credential in Keychain; the daemon does
  not read that item. The Mac app uses `com.bitomule.alondra.mac.auth` for refresh tokens and
  `com.bitomule.alondra.mac.service-pairing.v1` for pairing secrets. The app's token item uses
  `AfterFirstUnlockThisDeviceOnly`, pairing uses `WhenUnlockedThisDeviceOnly`.
  See [CLI account](../crates/alondra/src/account.rs), [Mac app](../apple/macOS/ViewsShipping/MacAppDelegate.swift),
  [token storage](../apple/Data/Account/Sources/Auth/KeychainRefreshTokenStorage.swift) and
  [pairing store](../apple/Data/Pairing/Sources/PairingStore.swift).
- **Device keys:** Service signing/agreement keys and local loopback identity keys authenticate
  and encrypt device traffic. These are separate from provider logins. Paired-device approval
  is local to the Mac. See [enrollment](../crates/alondra/src/service_setup.rs).
- **GitHub/Git:** `gh` and Git use their own configured credentials and helpers, including any
  SSH agent socket. Alondra invokes them with authority to read, close and merge PRs and publish
  diagnostic reports. See [GitHub runner](../crates/daemon-engine/src/domain_github.rs).
- **Agent environment and integrations:** Hosts pass a selected environment (including proxy,
  provider endpoint, GitHub and SSH settings), not the entire daemon environment.
  Claude and Codex receive `ALONDRA_WAIT_FILE`, the path to their own host's `wait.json`;
  commands can inherit it to report a wait. Without it the reporting command does nothing.
  `node_environment` can add variables; their values may persist in host ownership records.
  MCP servers and provider plugins have their own credentials and access. See
  [environment selection](../crates/daemon-engine/src/sdk_owner/backend.rs).
- **Optional direct APNs:** `~/.alondra/push.json` holds notification registration state;
  `~/.alondra/apns.json` names an Apple `.p8` private key file, team,
  key ID and topic. The daemon reads that key to send notifications when direct APNs is
  configured. See [push](../crates/daemon-support/src/push.rs).

## Connections

| Connection | Default and data involved | Source |
|---|---|---|
| Local CLI/domain API | HTTP on `127.0.0.1:47800` by default; configurable address/port. Local task/card commands and reads. | [Configuration](../crates/core/src/config.rs), [API](../crates/alondrad/src/domain_api.rs) |
| Device HTTP transport | TCP `47810`, all IPv4 interfaces by default, configurable. Service requests/responses are authenticated encrypted envelopes, not a public task API. | [Service listener](../crates/alondrad/src/service.rs) |
| iroh transport | UDP `47811` on IPv4/IPv6, with an available-port fallback; direct encrypted device streams and a relay learned from the service. Router mapping uses UPnP/NAT-PMP/PCP when available. | [Endpoint](../crates/alondrad/src/iroh_endpoint.rs) |
| Local discovery and permission probe | Bonjour `_alondra._tcp` advertises/discovers the Mac; a one-byte UDP probe to the gateway on port 9 checks Local Network access. | [Bonjour](../crates/core/src/bonjour.rs), [probe](../crates/daemon-support/src/local_network.rs) |
| Alondra Cloud (optional enrollment) | HTTPS `https://api.getalondra.com` by default, configured in `service.toml`. Registration, device routing, heartbeat, push delivery and relay discovery. Device content is sealed; the control plane receives routing/public identity/push metadata. | [Service config](../crates/core/src/service_config.rs), [relay](../crates/alondrad/src/service/relay.rs), [enrollment](../crates/alondra/src/service_setup.rs) |
| Account sign-in | Browser and HTTPS OIDC at `https://auth.getalondra.com/realms/alondra`, using discovered authorization/token/userinfo/revocation endpoints. A temporary loopback callback receives the CLI browser sign-in. | [CLI login](../crates/alondra/src/account.rs), [app OIDC](../apple/Data/Account/Sources/Auth/OIDCConfiguration.swift) |
| Providers and MCP | Claude Code/Codex contact their provider using the selected login and endpoint settings; MCP tools contact their configured services. Prompts, code and tool results can leave the Mac through these tools. | [Claude host](../agent-host/session.ts), [Codex host](../agent-host/codex-session.ts) |
| GitHub, Git remotes and PR images | `gh`/Git contact the configured host/remotes. The open-PR sweep schedules closure of registered PRs whose tasks are cancelled; the cancellation comment sent to GitHub contains the task title. PR preview fetching can download GitHub image attachments, including signed `private-user-images.githubusercontent.com` and recognized `raw.githubusercontent.com` image URLs. | [GitHub](../crates/daemon-engine/src/domain_github.rs), [open-PR sweep](../crates/daemon-engine/src/open_pr_sweep.rs), [images](../crates/daemon-engine/src/pr_images.rs) |
| Release checks and updates | HTTPS GitHub API `api.github.com/repos/bitomule/homebrew-tap/releases?per_page=100`; override `ALONDRA_UPDATE_URL`. Homebrew downloads packages when an update is requested or automatic updates are enabled. | [Release URL](../crates/core/src/update.rs), [updater](../crates/daemon-support/src/updates.rs), [formula](../deploy/brew/alondra.rb.in) |
| Diagnostic reports (when sent) | Uses `gh api` to upload the previewed, sanitized bundle to a secret gist and create an issue linking it. Anyone with the issue's gist URL can read it. Report retrieval downloads from `gist.githubusercontent.com`. Destination comes from relay bootstrap or `report.repo`. | [Report preparation](../crates/alondra/src/report/mod.rs), [sending](../crates/alondra/src/report/send.rs), [GitHub transport](../crates/alondra/src/report/github.rs) |
| Direct APNs (optional) | HTTPS `api.push.apple.com` or `api.sandbox.push.apple.com`, authenticated with the configured Apple key. Enrolled-service push normally goes through the relay instead. | [Push](../crates/daemon-support/src/push.rs) |

When a TUI view request receives no response, the CLI probes the same configured destination
with an empty change-feed request (`wait=0`) and a five-second availability decision deadline.
Any HTTP response confirms connectivity; the operation still uses the transport's normal
authentication and response validation. See [availability checks](../crates/alondra/src/client/availability.rs).

DNS, proxies, provider-specific endpoints and network connections made by Homebrew, Git,
repository commands or MCP servers depend on your configuration. This table is not a fixed
firewall policy. The [device protocol](iroh-protocol.md) describes encryption and pairing.

## macOS permissions

- **Background execution:** The LaunchAgent appears in Login Items & Extensions. Disabling it
  prevents launchd from keeping the daemon running. See [doctor's background-item check](../crates/alondra/src/doctor/system.rs).
- **Local Network:** The daemon/launcher and Mac app use the local network for discovery and
  device transport. macOS can ask for permission. The launcher has a stable installed path to
  retain that permission across Homebrew upgrades. See [daemon plist](../crates/alondrad/Info.plist),
  [Mac app plist](../apple/macOS/Info.plist) and [launcher](../crates/core/src/launcher.rs).
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
