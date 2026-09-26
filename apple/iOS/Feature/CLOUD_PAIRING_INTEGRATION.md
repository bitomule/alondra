# Cloud pairing integration

The app root now consumes `CloudPairingFeature` and replaces the local-daemon pairing form with account login and approved service pairing. The feature stores presentation state only. `CloudPairingOwner` owns AuthSession, private pairing attempts and the Keychain-backed ServicePairingClient. The QR relay URL is checked against the deployed service origin before any account bearer is requested or sent.

The phone displays its own comparison code throughout approval polling. Cancellation invalidates the feature generation, discards the attempt, clears local credentials and waits for cleanup before allowing another login. Restored approved pairings require the issuer's verified subject to match the stored account.

## Remaining integration gate

This branch must not merge until the shared service facade activates the approved identity and the root can mount migrated content. The approved screen currently remains a status screen; no local HTTP tabs are silently unlocked. The activation/deactivation callbacks in the process owner are the composition point for that facade. Replace their defaults with required construction arguments when composing the live root. Do not introduce a legacy HTTP bridge.

## Verification

The feature tests cover candidate submission remaining locked until approval and rejection of a stale approval after cancellation. Swift frontend parsing and `git diff --check` pass. The first Bazel test attempt failed before Swift compilation with ENOSPC (invocation eb4a77fc-d747-4a1c-88e1-274fe3490cca). Signed iOS build and MAV simulator verification remain required; no result is claimed from the failed build.
