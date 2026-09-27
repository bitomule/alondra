# Local Apple distribution preparation

This lane prepares private, verified local artifacts for iOS TestFlight and the
Mac App Store. It never calls App Store Connect, creates profiles/certificates,
exports private keys, installs the app, or uploads anything. It is separate from
Homebrew and from simulator/MAV builds.

## Existing rule mechanisms

The implementation uses the repository's pinned rules_apple 5.0.0-rc3:

- `ios_application` emits an IPA; `macos_application` emits a ZIP.
- `provisioning_profile`, `entitlements` and the
  `apple/build_settings:signing_certificate_name` flag control signing.
- `apple_bundle_version` obtains explicit version/build from `--embed_label`.
- `--ios_multi_cpus=arm64` selects the device platform. An arm64 simulator is not
  interchangeable with this build.

These mechanisms were checked in the installed pinned rule sources before
implementation (`apple/internal/rule_attrs.bzl`, `apple/versioning.bzl`,
`apple/internal/codesigning_support.bzl`, `doc/tutorials/ios-app.md`). Upstream
primary documentation: [iOS rules](https://github.com/bazelbuild/rules_apple/blob/5.0.0-rc3/doc/rules-ios.md),
[macOS rules](https://github.com/bazelbuild/rules_apple/blob/5.0.0-rc3/doc/rules-macos.md),
[signing](https://github.com/bazelbuild/rules_apple/blob/5.0.0-rc3/doc/common_info.md).
No generated Xcode project or fabricated xcarchive is involved.

## Prerequisites

Use a clean, committed, reviewed checkout in an isolated worktree, with Xcode,
Bazelisk and Python 3.11+ available. Coordinate the local build slot first. The
script uses one Bazel job. Never run it alongside the backend or MAV build.
Pass `--bazel-output-user-root` and `--bazel-output-base` to reuse the agreed local
cache instead of allocating another worktree cache. Both build and cquery receive
the same startup paths. Sharing a path does not authorize concurrent builds.

Obtain the appropriate profiles through the separately authorized ASC signing
workflow. Place the existing CMS profile files in ignored `signing/` paths:

| Destination | File | Bundle identifier |
| --- | --- | --- |
| iOS app | `Alondra_AppStore.mobileprovision` | `com.bitomule.alondra` |
| iOS extension | `Alondra_NotificationService_AppStore.mobileprovision` | `com.bitomule.alondra.NotificationService` |
| Mac App Store | `Alondra_Mac_AppStore.provisionprofile` | `com.bitomule.alondra.mac` |

The profiles must belong to team/prefix `658YBSK6MG`, be unexpired, exclude
development/ad hoc/enterprise devices, and include the selected existing Apple
Distribution certificate. An existing valid Mac Installer identity is also
required for the Mac package. Certificate and profile downloads are deliberately
outside this script. Never commit profiles or private keys.

Inspect next build numbers immediately before selecting version/build:

```sh
asc builds next-build-number --app 6816262826 --platform IOS
asc builds next-build-number --app 6816263014 --platform MAC_OS
security find-identity -v -p codesigning
security find-identity -v -p basic
```

The version is an explicit `x.y.z`; build is an integer 1–9999. The source's normal version is `0.1.1 (3)`; distribution receives the same
explicit version/build through the release command. ASC's initial App Store drafts were `1.0` at the
26 Sep inventory; align the App Store version before attaching a release build.
TestFlight can process a different marketing version without changing that draft.

## Preflight, then prepare

Replace the fingerprints with the existing identities returned above; the
script accepts fingerprints, not ambiguous certificate names. Preflight reads
only local identity metadata and existing profiles, and does not build.

```sh
python3 deploy/apple/prepare.py ios --version 0.1.0 --build-number 1 \
  --identity "$APPLE_DISTRIBUTION_SHA1" \
  --output /private/tmp/alondra-ios-distribution-1 --preflight-only

python3 deploy/apple/prepare.py ios --version 0.1.0 --build-number 1 \
  --identity "$APPLE_DISTRIBUTION_SHA1" \
  --output /private/tmp/alondra-ios-distribution-1

python3 deploy/apple/prepare.py macos --version 0.1.0 --build-number 1 \
  --identity "$APPLE_DISTRIBUTION_SHA1" \
  --installer-identity "$MAC_INSTALLER_SHA1" \
  --output /private/tmp/alondra-mac-distribution-1
```

The output must be a new directory outside the checkout. Failed attempts retain
their directory; use a new destination after diagnosis. There is no overwrite or
retry-upload command. Successful output is an IPA or signed PKG plus a manifest
containing source commit, explicit version, artifact SHA-256, selected signing
identity fingerprint and profile hashes. It does not contain profiles or keys.

Distribution is selected only by `--define=alondra_distribution=1`, supplied by
the script along with the explicit version label and signing identity. Without
that define, existing profiles, entitlements and versions are unchanged. Mac
builds in this initial lane are Apple Silicon; universal/Intel GUI distribution
is not claimed.

The script rejects a simulator IPA even when its filename looks correct. Both
iOS bundles must have the expected IDs/version, exact validated embedded
profiles, matching signer, device Mach-O platform, production APNs on the app,
and the shared Keychain group. The Mac artifact must have matching identity,
profile, version and sandbox/network/user-selected-file entitlements. Strict
nested code-signature verification runs before packaging. The Mac package's
Installer signature is checked after `productbuild`, followed by an exact SHA-1
comparison of the package signature's leaf DER certificate against the requested
Installer fingerprint. An issuer/root matching the requested fingerprint does not
satisfy this check. The installed `productbuild` also accepts an exact SHA-1
selector: this was verified with a real empty package while an expired certificate
with the same Common Name remained in the login keychain. The lane passes that
fingerprint, rechecks its validity before signing, and verifies the resulting leaf.
If another host cannot select by fingerprint, signing fails without approving an
artifact. `pkgutil` must explicitly report an Apple-issued trusted developer
signature; exit zero alone is insufficient (an untrusted status can also exit zero).

The Bazel ZIP/IPA is copied into a unique private staging directory before any
archive validation. That read-only snapshot is the only archive subsequently
validated, extracted and copied into the release output. A concurrent replacement
of the Bazel output path cannot change the bytes shipped after validation.

Failed commands leave a private diagnostic at the path reported in the error
(mode 0700 directory / 0600 log). It contains the tool name, exit status and capped,
filtered diagnostics. It never records environment variables, command arguments
or `security cms` stdout (which could contain a profile). Treat the local log as
private evidence; it is not included in the release manifest or uploaded.

## Verification and remaining release gates

```sh
python3 -m unittest discover -s deploy/apple -p 'test_*.py'
```

These tests serialize real plist and ZIP fixtures and exercise expired/wrong
profile, certificate/team/platform mismatch, simulator, absent/extra extension,
foreign payload and ZIP traversal/symlink rejection. They do not prove real CMS
validation, signing, a successful Bazel distribution build, package acceptance,
or runtime behavior inside the Mac sandbox. Those checks require real artifacts
and remain mandatory after independent review of this lane.

After preparation, complete MAV/runtime checks for the distribution build,
including Mac file import/export, Keychain/login, notifications and the iOS
extension. Then the orchestrator may use the existing ASC upload workflow with
the verified artifact. ASC processing, export-compliance answers, Beta App Review,
review access and external-group distribution are separate gates. A successful
local manifest is not an App Store or TestFlight approval.

## Independent-review corrections

After the initial implementation, review found a mutable Bazel archive could be
reread after verification, and that comparing only the Installer Common Name did
not prove its identity. Both are addressed above. Two additional tests replace a
Bazel IPA with a simulator IPA between validation and output copying, and reject
ambiguous Installer names / a wrong leaf even when the requested certificate is
later in the signature chain. These are packaging validation tests, not a claim
of cryptographic validation of synthetic certificate fixtures.

Local primary documentation checked for the fix: `man productbuild` (SIGNED
PRODUCT ARCHIVES: --sign selects Common Name), `man xar` (--dump-toc=- emits the
XML TOC) and `man pkgutil` (--check-signature checks validity/trust). The leaf parser
was also checked read-only against an existing local signed Memoir PKG: its exact
Installer fingerprint was accepted, a different fingerprint rejected. That
historical PKG returns an untrusted-certificate failure from pkgutil on this host;
it is not accepted by this lane and is not evidence of a successful new release.
No Apple build, package signing, ASC mutation or upload was performed for this fix.

## Real local signing probe, 26 Sep

Created the three missing App Store profiles with the existing distribution
certificate, and kept downloads in ignored signing paths. iOS preflight passed.
The initial Mac preflight correctly detected an expired same-name Installer
identity. A local empty package signed with the exact requested SHA-1 proved
selection of the current certificate without changing/exporting any key. Its XAR
leaf matches `15028A74350CC881DF8CF8956D0AC9D7DC9D02E9`.

The same package reports untrusted inside the restricted sandbox and an Apple-issued
chain outside it. Both invocations exit zero. The gate now checks the explicit
trust status as well as the exact leaf, using C locale. Six packaging tests pass,
including the observed misleading-success status. Probe evidence is private under
`/private/tmp/alondra-installer-identity-probe`; it is not an Alondra app build,
installation, upload or release approval.
