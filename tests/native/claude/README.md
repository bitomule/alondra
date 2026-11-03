# Official Claude native loopback test (opt-in)

This target runs official Agent SDK **0.3.300** and the unmodified Darwin arm64
Claude Code **2.1.300** against one owned HTTP/SSE model fixture.

After source review and reservation of the shared heavy slot:

```sh
bazelisk test //tests/native/claude:text_roundtrip_test --test_output=all --nocache_test_results
```

`//tests/native/claude:accounts_test` runs the same pinned SDK and native Claude with two
`CLAUDE_CONFIG_DIR` folders that link one fixture `~/.claude`, as Alondra prepares a provider
account's folder: it checks that the linked skills, agents, commands, rules, instructions and
settings load, that a session created with one folder resumes with the other under the same id,
and that every link survives a turn.

```sh
bazelisk test //tests/native/claude:accounts_test --test_output=all --nocache_test_results
```

The targets are `manual`, local and macOS arm64 only. It is outside ordinary test,
hook and release runs. Its separate lazy repository downloads the official
platform npm archive using the existing lock's exact SHA-512 integrity; it leaves
`no_optional=True` and the SDK lock untouched. The test reads only the unique
regular-file `claude` archive member, verifies its 249,556,880 bytes against
SHA-256 `475517caa611e3318a2f298fa071287ec98955538df78c58f709ecc01e5ae2d3`,
and copies it without patching, signing or invoking an installed executable.

The pinned Node artifact, native binary and closed SDK client bundle are copied
and hashed in a fresh fixture outside user homes. `/usr/bin/sandbox-exec` applies
`deny default` **before** either SDK/native launch: no Mach lookup, reads only
fixture/system runtime paths, writes only fixture state/diagnostics, process exec
only the copied Node/native, and outbound TCP only the retained model listener's
exact loopback port (Seatbelt only accepts `localhost:PORT` as a host; it admits
127.0.0.1 on that port and nothing else). Beyond the runtime, system libraries and
ICU data, the profile lets Node and Bun list `/` and read the metadata of each
ancestor of the runtime directory, which their startup and `realpath` need; no
other directory is listed or read. A sandboxed preflight first proves allowed fixture read/TCP
and denied read of an independently owned sibling canary plus denied connection
to a second retained owned listener. No real home, Keychain or credential file is
probed. Failure stops before query; there is no weaker sandbox/profile fallback.

Both SDK client and native receive an explicit environment: temporary HOME,
CLAUDE_CONFIG_DIR, TMPDIR, CLAUDE_CODE_TMPDIR (Claude otherwise writes under
`/tmp/claude-UID`) and cwd, fixed PATH/TZ/LANG, public synthetic API key and
owned loopback BASE_URL. SDK `env` replaces ambient credentials/proxies; custom
spawn delegates the real SDK command/args to the actual pinned native child and
asserts the complete environment: the fixture's plus exactly the three variables
SDK 0.3.300 adds (`CLAUDE_CODE_ENTRYPOINT=sdk-ts`, `CLAUDE_AGENT_SDK_VERSION=0.3.300`,
`CLAUDE_CODE_SDK_READS_SESSION_STATE=1`). The client checks its own environment
before loading the SDK, then again after: loading SDK 0.3.300 sets
`NoDefaultCurrentDirectoryInExePath=1` on `process.env`, and that is the only change
allowed. Preset native tools/prompt and user/project/local
sources remain enabled; no `--bare`, simple mode or permission bypass is used.
The profile explicitly suppresses nonessential traffic and auto-updates; it does
not represent default-account feature/settings behavior.

The HTTP fixture requires the actual user prompt's unique marker and synthetic
key, scripts one Messages API SSE text response, and rejects unexpected requests.
The actual SDK must report native init version, assistant text and successful
result matching that response. A real `spawn`/`exit` barrier requires exactly one
SDK-owned native child and actual zero exit before scratch removal. Query-close,
process-stop and exit errors are retained independently; the primary failure is
preserved. Timeouts diagnose hangs and fail the run. Unresolved native cleanup
retains scratch and fails; parent termination alone is never native exit evidence.

Python handles SIGINT/SIGTERM and other exceptions through the same owned-client
teardown. Signals are deferred across spawn/owner registration and cleanup;
stop failures cannot skip the actual wait, and repeated signals cannot replace
the first failure. Client exit and the native child report remain independent.
Scratch is removed only after every started client is reaped and native cleanup
is independently known (or no query client started).

The client leads its own process group, and the SDK spawns native Claude without
`detached`, so Claude and whatever it forks share that group. The harness observes
the client's exit without reaping it (kqueue `NOTE_EXIT`, then `waitid` with
`WNOWAIT`), sends SIGKILL to the group while the client is still an unreaped zombie
(POSIX.1-2024 §4.17: its PID, and so the group number, cannot be reused), waits for
every live member's exit and only then reaps the client. No group is ever signalled
after its leader is reaped. A member still alive after a clean client exit is a
cleanup failure naming its PID and start time. A grandchild that calls `setsid`
leaves the group and is out of reach of this guard.

`//tests/native/claude:client_lifecycle_test` runs in every `bazel test //...` and in
MUSTS. Real owned Python stand-ins cover KeyboardInterrupt, SIGTERM, a reported stop
error/repeated signal, normal client exit without native proof, failed spawn, and
a client that dies before its `finally` (or exits cleanly) after its Claude
(`agent-host/descendant-fake-claude.py`) left a live descendant in the group: that
descendant must not survive, checked by PID plus start time and by its completion
lock. It imports the same fixture helper and has no SDK, Node or native archive
dependency; it establishes no native harness behavior.

Bazel undeclared outputs retain the report/request journal and diagnostics. This
is a real-native-harness / fake-model text test. Skills, permissions, hooks,
background work, retries, Rust/store policy, subscription logout, Keychain,
OAuth/entitlement behavior and production activation need separate acceptance.

## Artifact and license profile

SDK/native version and binary digest come from the official SDK manifest commit
`c6733ae1276880507ca0c5b053ae18fb0df090d6` (build 2026-10-14). The archive integrity
matches `agent-host/pnpm-lock.yaml`. Anthropic distributes the SDK/native under
its linked proprietary agreements; this test adds no redistribution or Brew
payload. Review the [official legal terms](https://code.claude.com/docs/en/legal-and-compliance)
and [SDK overview](https://code.claude.com/docs/en/agent-sdk/overview) for the
applicable commercial and third-party login terms. The fixture performs no login
or real provider/account request. This profile does not establish API fallback
or subscription authorization for Alondra.
