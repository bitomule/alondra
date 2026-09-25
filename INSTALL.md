# Installing Alondra

Written for an agent (Claude Code) to execute on a fresh Mac, step by step. Every step ends with
a **Check**: run it and compare with **Success**. Do not go to the next step until the check
passes. Where a step needs the human (Apple account, iPhone, a secret key), stop and ask.

What gets installed:

| Piece | Where |
|---|---|
| `alondrad`, the daemon (24/7 HTTP server, SQLite) | `~/.local/bin/alondrad`, run by launchd |
| `alondra`, the CLI/TUI and the nodes' MCP server | `~/.local/bin/alondra` |
| Settings, database, logs, APNs key | `~/.alondra/` |
| Claude Code plugin `alondra` | this repo, registered as the marketplace `alondra`; alondrad loads it only into the sessions it launches, never into every session |
| iOS app (on the iPhone) and Mac app | built here with Bazel, signed with your Apple team |

Nothing comes from Homebrew or TestFlight except the generic tools in step 1. Keep the
defaults for the install folder (`~/.local/bin`) and the port (`47800`): the plugin's hooks
and its `bin/alondra` wrapper point to them.

Run every command from the root of this repo unless a step says otherwise.

## 1. Requirements

- macOS 26 or later on Apple Silicon.
- Xcode 26 or later, opened once (licence accepted), with its command line tools selected.
- [Homebrew](https://brew.sh) only to get the generic tools below.
- `bazelisk` (it downloads the Bazel version pinned in `.bazelversion`). You do **not** need
  Rust for Alondra: Bazel's `rules_rust` downloads its own toolchain.
- Claude Code with background sessions (`claude --bg`), logged in.
- `tmux` (Alondra wakes stopped nodes through it), `gh` (logged in, with access to
  `bitomule/alondra`), `jq`, and `tuicr` 0.26 or later (the TUI opens a
  PR's diff in it; `alondra setup` installs it if it is missing).

```bash
xcode-select --install 2>/dev/null; sudo xcodebuild -license accept
brew install bazelisk tmux gh jq tuicr
```

**Check**

```bash
uname -m; sw_vers -productVersion; xcodebuild -version | head -1; xcode-select -p
bazelisk --version; tmux -V; gh auth status 2>&1 | head -3; jq --version; tuicr --version
claude --version; claude --help | grep -c -- '--bg'
```

**Success:** `arm64`; macOS `26.x` or later; `Xcode 26.x` or later; a path inside
`Xcode*.app/Contents/Developer` (not `/Library/Developer/CommandLineTools`: if so, run
`sudo xcode-select -s /Applications/Xcode.app`); `bazel 8.4.2` (run it from the repo, it reads
`.bazelversion`); `tmux 3.x`; gh logged in (if not: `gh auth login`); `jq-1.x`; a Claude Code
version and a number ≥ 1.

## 2. Build and install alondra and alondrad

The first build downloads Bazel, the Rust toolchain and the crates: several minutes.

```bash
make install
```

`make install` builds the CLI, the daemon and the Mac app, then copies `alondra` and
`alondrad` to `~/.local/bin`. Put that folder on `PATH` if it is not:

```bash
grep -q '.local/bin' ~/.zshrc || echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
export PATH="$HOME/.local/bin:$PATH"
```

**Check**

```bash
command -v alondra alondrad; alondra --help | head -3; alondra config
```

**Success:** both paths in `~/.local/bin`; the help text; `alondra config` prints the settings
in effect (defaults at this point: `port = 47800`, `listen = 127.0.0.1`).

## 3. Configure: `alondra setup`

`alondra setup` writes `~/.alondra/config.toml` (and `apns.json` in step 4), installs the
launchd service (`~/Library/LaunchAgents/dev.alondra.alondrad.plist`), adds this repo as a
Claude Code plugin marketplace (alondrad finds the plugin there and loads it into each session
it launches) and, at the end, shows the pairing QR. It does not enable the plugin for every
Claude session; if an older install did, it turns that off with `claude plugin disable
alondra@alondra --scope user`, which sets `"enabledPlugins": {"alondra@alondra": false}` in
`~/.claude/settings.json`.
It never asks when it gets `--no-input`, and `--dry-run` shows everything it would do.

Values to pass:

- `--name`: the human's first name. The nodes address them by it.
- For the iPhone to reach this Mac on the same Wi-Fi: `--listen 0.0.0.0` and
  `--public-url http://<LocalHostName>.local:47800`, where `<LocalHostName>` is
  `scutil --get LocalHostName`. Anyone on that network can reach the port; use it on a network
  you trust. For access from outside home see step 6 instead.

Leave pairing for step 5 (`--skip-pair`): the app is not on the iPhone yet.

```bash
H=$(scutil --get LocalHostName)
alondra setup --no-input --name "<first name>" --listen 0.0.0.0 \
  --public-url "http://$H.local:47800" --skip-pair --dry-run
```

Read the plan it prints: one `would write …config.toml`, one `would write …plist`, `launchctl
bootstrap` and `claude plugin marketplace add <this repo>` (plus `claude plugin disable
alondra@alondra --scope user` on a Mac where an older install enabled it). If it says `Not touching anything, because:` it found an existing
installation: show the lines to the human before using `--force`. Then run the same command
without `--dry-run`.

**Check**

```bash
alondra config; curl -s http://127.0.0.1:47800/v1/health | jq .config; alondra doctor
```

**Success:** `alondra config` shows your `name`, `listen = 0.0.0.0`, `public_url`; the health
answer has the same values; `alondra doctor` shows ✓ for `config`, `alondrad`, `launchd`,
`claude`, `plugin`, `tmux`, `tuicr`. Expected at this point: `!` for
`apns` (step 4) and `pairing` (step 5). Only the sessions Alondra launches load the plugin;
any other Claude session never talks to Alondra.

## 4. Notifications with your own Apple developer account

This needs a paid Apple Developer Program membership. Pick a bundle ID prefix you own, e.g.
`com.<you>.alondra`. Three IDs are used:

| What | Bundle ID |
|---|---|
| iOS app | `com.<you>.alondra` |
| Notification service extension | `com.<you>.alondra.NotificationService` |
| Mac app (not signed with a profile, no portal work) | `com.<you>.alondra.mac` |

### 4a. Bundle IDs, push, certificate, device, profiles

With the [`asc`](https://github.com/rudrankriyam/App-Store-Connect-CLI) CLI, if installed and
logged in (`asc auth status`):

```bash
asc bundle-ids create --identifier com.<you>.alondra --name "Alondra" --platform IOS
asc bundle-ids create --identifier com.<you>.alondra.NotificationService --name "Alondra Notification Service" --platform IOS
asc bundle-ids capabilities add --bundle <app bundle resource id> --capability PUSH_NOTIFICATIONS
asc devices register --name "<iPhone name>" --udid <iPhone UDID> --platform IOS   # UDID: xcrun devicectl list devices --columns hardwareProperties.udid
asc certificates list --certificate-type DEVELOPMENT                               # reuse one whose private key is in this Mac's keychain
asc profiles create --name "Alondra Development" --profile-type IOS_APP_DEVELOPMENT \
  --bundle <app bundle resource id> --certificate <cert id> --device <device id>
asc profiles create --name "Alondra NotificationService Development" --profile-type IOS_APP_DEVELOPMENT \
  --bundle <extension bundle resource id> --certificate <cert id> --device <device id>
asc profiles download --id <app profile id> --output signing/Alondra_Development.mobileprovision
asc profiles download --id <extension profile id> --output signing/Alondra_NotificationService_Development.mobileprovision
```

(`asc bundle-ids list` gives the resource IDs. If the `asc-signing-setup` skill is available
it covers these commands in detail.)

Without `asc`, on https://developer.apple.com/account/resources: Identifiers → + → App IDs →
the two IDs above, with **Push Notifications** checked on the app one; Devices → + → the
iPhone's UDID; Profiles → + → iOS App Development, once per ID, with your Apple Development
certificate and the iPhone; download both into `signing/` with the file names above.

A signing certificate with its private key must be in the login keychain. If there is none:
Xcode → Settings → Accounts → your team → Manage Certificates → + → Apple Development.

**Check**

```bash
security find-identity -v -p codesigning | grep "Apple Development"
for p in signing/*.mobileprovision; do security cms -D -i "$p" | plutil -extract Entitlements.application-identifier raw -; security cms -D -i "$p" | plutil -extract Entitlements.aps-environment raw -; done
```

**Success:** at least one `Apple Development` identity; for each profile `<TEAMID>.<bundle id>`
matching the table, and `development` for the app profile (the extension profile has no
`aps-environment`; an error on that line for it is fine).

### 4b. APNs key

On https://developer.apple.com/account/resources/authkeys: Keys → + → name "Alondra APNs",
check **Apple Push Notifications service (APNs)** → Continue → Register → Download. It can be
downloaded only once. Note the **Key ID** (10 characters) and the **Team ID** (top right of
the portal, or `Membership`).

```bash
mkdir -p ~/.alondra && mv ~/Downloads/AuthKey_<KEYID>.p8 ~/.alondra/ && chmod 600 ~/.alondra/AuthKey_<KEYID>.p8
alondra setup --no-input --apns-key-path ~/.alondra/AuthKey_<KEYID>.p8 \
  --apns-key-id <KEYID> --apns-team-id <TEAMID> --apns-topic com.<you>.alondra --skip-pair
```

It writes `~/.alondra/apns.json` (mode 600) and restarts the daemon so it loads the key.

**Check**

```bash
alondra doctor | grep apns
```

**Success:** `✓ apns       apns.json for com.<you>.alondra` and `✓ apns push  Apple accepts
the key (test token rejected as expected)`. `✗ apns push  Apple rejects the key:
{"reason":"InvalidProviderToken"}` means the key ID, team ID and key file do not belong
together: fix them and rerun the same setup command with `--force`.

### 4c. Signing file and the apps

```bash
cp signing/signing.example.bzl signing/signing.bzl
```

Edit `signing/signing.bzl` (git-ignored): `IOS_BUNDLE_ID = "com.<you>.alondra"`; keep the
other lines, which derive `.mac` and `.NotificationService` from it and name the two profiles
downloaded in 4a. The team comes from the profiles.

Build and install the iOS app on the iPhone (USB or same Wi-Fi, unlocked, Developer Mode on:
Settings → Privacy & Security → Developer Mode):

```bash
bazelisk build //apple:AlondraiOS --ios_multi_cpus=arm64
IPA=$(bazelisk cquery --output=files //apple:AlondraiOS --ios_multi_cpus=arm64 2>/dev/null | grep '\.ipa$')
rm -rf /tmp/alondra-ipa && unzip -q "$IPA" -d /tmp/alondra-ipa
xcrun devicectl list devices
codesign -dvv /tmp/alondra-ipa/Payload/Alondra.app 2>&1 | grep -E '^Identifier|TeamIdentifier'
xcrun devicectl device install app --device <iPhone name or id> /tmp/alondra-ipa/Payload/Alondra.app
```

`--ios_multi_cpus=arm64` is required: without it Bazel builds for the simulator, unsigned. The
`codesign` line must show your bundle ID and your team, not `TeamIdentifier=not set`.

Build and install the Mac app in `~/Applications` (it opens at the end):

```bash
make mac
```

**Check**

```bash
xcrun devicectl device info apps --device <iPhone> 2>/dev/null | grep -i alondra
ls ~/Applications/Alondra.app
```

**Success:** the iPhone lists `com.<you>.alondra`; the Mac app exists and is open. On the
iPhone, open Alondra and allow notifications. If iOS says the developer is untrusted:
Settings → General → VPN & Device Management → trust it.

## 5. Pair the iPhone

```bash
alondra pair
```

It prints a QR with the public URL and a one-time code (10 minutes, one use). On the iPhone:
Alondra → Settings → Pair with Mac → scan. The Mac app's pairing window shows the same QR.

**Check**

```bash
alondra devices; alondra doctor
```

**Success:** the iPhone is listed; `alondra doctor` is all ✓ and exits with code 0. Last
proof: `claude --bg --name "test/alondra" -- "Use the alondra ask tool to ask me yes or no."`
makes the question appear on the iPhone with a notification.

## 6. Remote access (optional)

- **Same Wi-Fi:** what step 3 set up (`listen 0.0.0.0`, `http://<LocalHostName>.local:47800`).
- **Outside home: Tailscale.** Install Tailscale on the Mac and the iPhone, same tailnet. Serve
  the daemon over HTTPS (the iOS app only allows plain HTTP to local names and IPs, so a
  `*.ts.net` name needs HTTPS). The daemon can go back to listening only on this Mac:

  ```bash
  tailscale serve --https=443 --bg localhost:47800     # accept enabling HTTPS if it asks
  alondra setup --no-input --force --listen 127.0.0.1 --public-url https://<mac>.<tailnet>.ts.net --skip-pair
  alondra pair
  ```

  **Check:** from the iPhone with Wi-Fi off, the app loads the fleet;
  `curl -s https://<mac>.<tailnet>.ts.net/v1/health | jq .ok` from any tailnet device.

## 7. Troubleshooting: `alondra doctor`

`alondra doctor` (or `--json` for agents) prints one line per check, and under each failure the
command that fixes it. It exits with 1 when any check is ✗.

| Check | ✗ / ! means | Fix |
|---|---|---|
| `config` | `~/.alondra/config.toml` missing (! defaults) or invalid (✗) | `alondra setup`; or fix/remove the file |
| `alondrad` | ✗ the daemon does not answer on the port. ! it answers with other settings than `config.toml` | `alondra setup`, `tail ~/.alondra/alondrad.log`; ! → `launchctl kickstart -k gui/$(id -u)/dev.alondra.alondrad` |
| `hooks` | the port is not 47800, so the plugin's hooks miss the daemon | remove `port` from `config.toml` and `ALONDRA_PORT` |
| `launchd` | ✗ service not loaded; ! loaded but stopped (usually a crash at start) | `alondra setup`; read `~/.alondra/alondrad.log` |
| `claude` | Claude Code is not on `PATH` | install Claude Code |
| `plugin` | ✗ the marketplace is not registered, so alondrad cannot load the plugin into its nodes; ! the plugin is enabled for every Claude session | ✗ → run from the repo: `claude plugin marketplace add "$PWD"`; ! → `alondra setup` (or the `claude plugin disable … --scope …` doctor prints) |
| `tmux` | tmux missing: stopped nodes cannot be woken | `brew install tmux` |
| `apns` | ! no `apns.json`: no notifications. ✗ invalid file or the `.p8` is missing | step 4b |
| `apns push` | Apple rejects the key (403) or answers oddly | check key ID, team ID and topic; rerun the step 4b setup command with `--force` |
| `pairing` | no paired device | step 5 |

Other problems:

- **`setup` says `Not touching anything, because:`**: values in `config.toml`/`apns.json`
  differ from the flags, or another launchd plist already runs `alondrad`. Read each line; use
  `--force` to overwrite, or `--from-plist <old plist>` to migrate an old service.
- **The build fails on signing** (`signing/signing.bzl not found` is only a warning):
  `signing/signing.bzl` names a profile file that is not in `signing/`, or the profile does
  not include this iPhone or your certificate. Redo 4a.
- **The iPhone cannot reach the Mac on Wi-Fi:** `curl http://<LocalHostName>.local:47800/v1/health`
  from the Mac must answer; allow incoming connections to `alondrad` if the macOS firewall
  asks; the iPhone must allow Local Network access for Alondra (Settings → Privacy).
- **Logs:** `~/.alondra/alondrad.log`.
