# Claude Agent SDK laboratory host

This laboratory host is not selected by the installed daemon or UI. `Session` calls the pinned Anthropic SDK `/core` entrypoint; tests use an external protocol-speaking child without calling Anthropic or reading a user's Claude profile. The Rust controller → persistent Unix host → SDK → fake native → store vertical passed all 15 uncached tests in invocation b6f11067-a455-42df-8570-c5d40393cc35 on the earlier 0.2.84 composition. Separate component RED/GREEN pairs cover native read-end cleanup, the outbox flush wakeup and retry telemetry classification. The separate d54b7e9c/0.2.85 delivery passed its required build (53 targets), general tests (69 passed: 29 executed / 40 cached) and host tests (11 passed, all executed); the implementation record identifies those runs. Production activation, authenticated adoption, performance and persistent SEA distribution remain unproved.

The query explicitly requests user/project/local settings, the Claude Code system-prompt and tools presets, `permissionMode: auto` (an approved plan returns to auto) and host permission callbacks: only what the auto-mode classifier cannot decide reaches the user. Per-query environment snapshots never mutate `process.env`. Native login remains outside the host. This probe neither reads credentials nor implements an API-key fallback.

Run only through Bazel:

```sh
bazelisk test //agent-host:sdk_test //agent-host:frame_test //agent-host:sea_test --test_output=errors
```

`sdk_test` executes actual SDK initialization, streaming input and permission/control transport against the fake Claude process. It covers permission denial/cancellation/late answers, concurrent environment isolation, interrupt, typed startup failure, authentication progress versus failure, assistant error, success with `is_error`, and EOF with an unresolved later input. `frame_test` verifies strict bounded JSON framing and exact-payload command hashes using the same compact Rust fixtures. Both tests declare canonical `//crates/agent-protocol:fixtures/...` data labels and read them from Bazel runfiles; no fixture copies live under `agent-host`. The fixture migration changes paths only and preserves every existing assertion and predicate. Error catalog tests prove adapter normalization, not native-provider behavior. The in-memory input set tracks outstanding responses only: a result correlation, including `user_message_uuid`, does not establish durable acceptance or `InputConsumed`. In particular, the SDK can attach that field to delivery/startup failures. Terminal policy remains a later Rust/domain gate.

`sea_test` copies the standalone binary to a disposable HOME, gives it an empty `node_modules` and PATH without JavaScript runtimes/package managers, adds hostile `NODE_PATH`/`NODE_OPTIONS`, denies network via the macOS sandbox, and launches a Python fake native Claude executable. It verifies initialized/result observations, a missing executable diagnostic, the payload SHA256 and the ad-hoc signature. No downloaded native Claude package is selected. The installed Claude executable remains an external prerequisite.

The build graph pins Node 24.19.0, rules_js, rules_ts, pnpm, TypeScript, esbuild, SDK and peer dependencies. Bazel fetches integrity-locked archives; dependency install scripts and home npm configuration are disabled. esbuild's macOS arm64 binary is an explicit build dependency. The CJS bundle inlines non-builtin imports, checks its static external closure and writes a metafile; the standalone offline smoke is the runtime closure check. Node's SEA blob disables code cache/snapshots and external Node execution arguments. Build tooling and SDK sources are not vendored or shipped from this repository.

Remaining production gates include daemon integration of the persistent framed IPC, atomic Rust Task/card composition, journal lifetime bounds, authenticated process adoption/restart, mandatory hooks, native skills/plugins/permissions parity, authenticated subscription/logout, supported CLI version range, Node 26 comparison, minimum macOS, reproducible release signing/manifest/extraction, notices/SBOM, budgets and installed upgrade rehearsals. An ad-hoc signed lab binary proves neither Developer ID distribution nor provider parity. The SDK license remains Anthropic's commercial license; packaging is not permission to relicense it.

## Hosts across a release

A daemon of a new release adopts the live hosts the release before launched: their turns and background commands go on, and once a node rests (no turn, no work of Claude's own, nothing on its way) a host of the new release that resumes the same session takes its place. The daemon never reads the old host's files: Homebrew removes the old release once the new one is installed. It expects the host as its retained manifest says it was launched.

What keeps an old host adoptable, each with its test:

- **A new action names a new capability.** Every action has the capability that carries it, the same in `ACTION_CAPABILITIES` (`frame.ts`) and `Action::capability` (Rust), checked against `//crates/agent-protocol:fixtures/action-capabilities.json` by `frame_test` and `agent_protocol_test`. The daemon never sends an action its host did not announce in its hello: it refuses it before any offer, since a host of an earlier release cannot even read an action it does not know.
- **Changing a frame that exists takes a new `PROTOCOL_VERSION`**, and the daemon keeps speaking the version before to the hosts it adopts. The shared fixtures are the frozen frames of the version: both sides read and write them byte for byte.
- **The SDK a host runs is one the daemon reads** (`ADOPTABLE_SDK_VERSIONS`), and **its release is not older than `OLDEST_ADOPTABLE_HOST_RELEASE`**. A release that fixes a host bug no node should keep until its next rest raises it to itself. A host that fails either is ended and replaced by one that resumes its session, as every host was before.
- The release's upgrade rehearsal (`deploy/brew/upgrade-rehearsal.py`, case `sdk-upgrade`) runs the published host of the release before under the new daemon, with a background command that must survive the upgrade.

## Codex process supervision

The Codex host ends when its native session ends, including planned close and force-stop.
Native termination tries SIGTERM and escalates to SIGKILL after one second. Stdout drains through
EOF before the exit observation and unresolved RPC rejection, with a one-second bound for pipes
held by descendants. The host retains the native exit in its journal and ends its own process
group with SIGKILL while still holding its ownership lock. Native commands cannot keep serving
after that lock is released.

Failed sessions get one second to flush operations and terminal evidence; an independent
two-second deadline bounds cleanup. Normal completion lets the owner acknowledge the terminal
cursor: five seconds without an advancing ACK starts cleanup, with a six-second watchdog for
stuck cleanup. Only validated, persisted ACKs advancing toward that fixed cursor renew those
deadlines. Duplicate ACKs do not. Terminal ACK, owner disconnection or explicit host shutdown
releases the wait. Claude's failure-only hook keeps its existing ACK behavior.

The daemon may see kernel native death before the host drains stdout. It sends TERM and CONT
to the bound host and gives it five seconds to finish that drain before KILL. The timeout only
triggers escalation; replacement still waits for verified exit and cleanup. Retained receipts
and observations distinguish offered, refused, absent and uncertain inputs before the same
native thread continues.

`CodexWorker::native_liveness()` exposes `NotStarted`, `Alive(Member)` or `Lost` independently of
the control socket. `Member` includes the kernel PID and birth identity; zombies are dead even
when `kill(pid, 0)` succeeds. The host's sole direct child is its stdio app-server; native tools
descend from it. A replacement opens it lazily on its first input, so an unopened host is
`NotStarted`. After authenticated adoption fences the old owner, retained Start/Offer admission
receipts establish possible startup even when no observation of this host has reached SQL.
Committed progress supplies that evidence after journal compaction. Cold controls do not count
as native startup. The owner checks this signal before dispatch and while idle, including for
adopted older hosts. This measures availability, not task progress.

Every launched host has an independent exit reaper. Dropping its owner leaves healthy work
alive; the reaper retains the child, sweeps its group while the unreaped leader still reserves
its PID, and calls wait. Explicit waits share that status and cleanup. Exit observation does
not wait for descendant cleanup. Adopted hosts use the authenticated peer's birth identity;
the adopting daemon cannot reap another process's child. Forced cleanup snapshots identities
under a live host or retained native anchor before shutdown, kills and waits those captured
processes, and refuses replacement if cleanup is unresolved.

The host deliberately does not restart its own app-server. A single replacement authority lets
the daemon quiesce the old writer and reconcile durable evidence before resuming the thread;
restarting inside the host would add another recovery path around uncertain input handoff.
No host-protocol frame, action, capability or version changes here.

## Antigravity host

`alondra-antigravity-host` runs one Alondra node on Antigravity CLI. It is the persistent host
with `AntigravitySession` (`antigravity-session.ts`) in place of Claude's or Codex's session, and
announces the SDK version `antigravity-acp-1`. Its capabilities are `streaming_input`,
`exact_decisions`, `owner_attach`, `commit_ack`, `interrupt`, `force_stop`, `set_model` and
`set_permission_mode`. It runs nodes only, never the orchestrator.

**Process.** The host's only child is `agy acp` (with `--gemini-dir <folder>` for a named account),
started in the node's worktree inside the host's own process group, as Codex's app-server is, so
the daemon's liveness check and reaper see it as the host's native child. It speaks the Agent Client
Protocol: JSON-RPC 2.0, one message per line on stdin and stdout. agy's stderr is drained and never
kept. The host sends `initialize` as the client `alondra` with no file system and no terminal, and
refuses the session (`native_incompatible`) when agy names another agent, speaks a protocol other
than 1 or cannot resume sessions. It reads agy's version from `initialize`'s `agentInfo.version`,
never from a second process: a version it cannot read is `native_version_unreadable`, and one
older than 1.5.12 is `native_version_unsupported`. An API key account then sends `authenticate`
with `gemini-api-key` and a Cloud project account with `agent-platform`. A Google or Enterprise
account sends none: agy uses the sign-in it keeps, and `authenticate` with an OAuth method would
open a browser sign-in and wait for it.

**Sign-in refusal.** A signed-out account shows as JSON-RPC `-32000` answering `session/new`,
`session/resume` or `authenticate`, and a key agy refuses as `-32602` answering `authenticate`; a
missing key file is the same refusal before agy starts. Nothing was offered to agy: every queued
input is refused as never offered (`authentication_required`), the account is reported blocked,
and the session ends.

**Resume.** The host keeps `native-sessions/<Alondra session id>` beside its state folder (`0700`
folder, `0600` file written atomically, refused when it is a link, another user's or has another
mode), holding the ACP session id with the account, access and agy version it ran on. A later host
sends `session/resume` only for the same account and access, and never `session/load`. For another
account or access, when agy refuses the resume with any error but `-32000` (agy answers an
unknown session with a generic one), or when a resumed node has no stored session, it opens a new
session and puts the task's handoff ahead of its first prompt. A node's first launch, with no stored
session, opens a new session and takes the handoff the daemon already gave it. A new session whose
first prompt carries the node's instructions or handoff is stored only once that prompt is sent, so
a later host never resumes a session that never got them.

**Session.** `session/new` and `session/resume` carry Alondra's MCP server, `alondra`, with the
node's role environment (`ALONDRA_PROVIDER=antigravity`); a node without it fails
`alondra_mcp_missing`. The node's instructions go in the new session's
`_meta.antigravity.systemPrompt.append` when `initialize` advertises it, and otherwise ahead of the
new session's first prompt; a resumed session already holds them. The host then sets the model and
after it the thinking level with `session/set_config_option`, from the options the session
reported (a level the model does not offer is not set), and the mode with `session/set_mode`:
Alondra's `default` is agy's `auto` when the session offers it, else `default`; `plan` is `plan`;
`dontAsk` is `default` with every permission request but Alondra's own tools rejected by the host. Model and mode changes
apply the same calls to the live session.

**Turns.** One `session/prompt` is open at a time; an input offered during a turn waits until agy
answers it, as agy's own steering of a running turn cannot name the input it took. The host
journals `alondra/sessionReady`, `alondra/promptSent` once the prompt is written, every
`session/update` as agy sent it, and `alondra/promptEnded` with agy's `stopReason`, or
`alondra/promptFailed` with the JSON-RPC error's code and structured `data`, never its message.
A `session/request_permission` becomes a card that lists its options; the answer selects the
option the decision names, and Alondra never selects `allow_always` or `reject_always`. A request
with neither an `allow_once` nor a `reject_once` option is a question, asked with its options'
names as answers. In `auto` and `dontAsk` modes, a request for one of Alondra's own MCP tools is
allowed once without a card; the tool is known by its `name`, never by its title, which agy writes
from the call (a command's title is its command line). Answers the host gives by itself are not
kept, so they never count against the decisions a session keeps.

agy reports an upstream failure such as a rejected key as an agent message in a turn that ends
`end_turn`; only a JSON-RPC error answering the prompt is `promptFailed`.

**Supervision.** An interrupt sends `session/cancel` and answers every pending permission request
`cancelled`; agy ends the turn by answering the prompt `cancelled`. Force stop cancels, then closes.
Closing sends `session/close` when agy advertises it, waits for it at most half a second, then sends
SIGTERM to agy and SIGKILL a second later. When agy exits, its last output is drained as for Codex
(an output that closes is the failure only when agy is still alive a second later), pending
requests settle `cancelled`, queued inputs are refused as never offered, and an exit nobody asked
for is journaled as a lost transport before the exit itself. The host ends with its session and
then ends its own process group, which ends whatever agy started. No timer ends a session that hangs once it is open; only its opening is bounded.

**Environment and TMPDIR.** agy's environment is the host's native environment without any Google
credential it could inherit (`GEMINI_API_KEY`, `GOOGLE_API_KEY`, `GOOGLE_APPLICATION_CREDENTIALS`,
`GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_LOCATION`, `GOOGLE_CLOUD_QUOTA_PROJECT`,
`GOOGLE_GENAI_USE_VERTEXAI`, `AGY_ADC_AUTH`, `GOOGLE_GEMINI_BASE_URL`, `GEMINI_HOME`), plus
`AGY_CLI_DISABLE_AUTO_UPDATE=1` and the account's own:

| Access | Set |
|---|---|
| a named account | `GEMINI_HOME` and `--gemini-dir`, its folder; for Google and Enterprise also `AGY_ACP_FORCE_FILE_STORAGE=1` and `SSH_CONNECTION=127.0.0.1 0 127.0.0.1 0`, so its sign-in stays in its folder |
| API key | `GEMINI_API_KEY` |
| Enterprise | `GOOGLE_CLOUD_QUOTA_PROJECT` and `GOOGLE_CLOUD_LOCATION` when set |
| Cloud project | with a key, `GOOGLE_API_KEY`; otherwise `AGY_ADC_AUTH=true` and `GOOGLE_APPLICATION_CREDENTIALS` when a credentials file is set; and the project and location |

A key is read from the account's key file, opened without following links and refused unless it is
a small regular `0600` file of the user's; a missing key is `authentication_required`. The key goes
only into agy's environment: never into the MCP server's, an observation, the host's journal or an
error. `TMPDIR` is a `0700` folder of the host's own inside its state folder, where agy unpacks its
runtime, removed when the session ends however it ends.

**Not offered.** MCP status, reconnection and sign-in, compaction and background work have no ACP
method: the host does not announce them and refuses their calls.

`antigravity_session_test` runs the session against `fake-agy-acp.mjs`, a fake ACP agent run by the
test's own Node, and needs no other program.
