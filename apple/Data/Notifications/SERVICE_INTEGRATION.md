# Service notifications wiring

The app and extension accept only a Mac-signed `kind: push` envelope. The
outer APNs binding, authenticated inner binding, current approved identity,
recipient and expiration must match. Plaintext daemon pushes cannot create
routes or actions. The app decrypts again when an action is delivered.

Root integration requirements:

- Add AlondraNotificationActions + AlondraNotificationPayload to AppComposition;
  add AlondraNotificationActions to the notification extension library.
- Provision the same explicit `keychain-access-groups` entry on iOS app and
  extension and put its fully expanded value in `AlondraPushKeychainAccessGroup`
  in both Info plists. Do not derive it from the bundle identifier alone.
- At approved service activation resolve a PushBindingStore binding from
  relay URL, owner subject, Mac ID, device ID and credential revision. Returning to
  an existing scope retains its binding; a distinct scope receives a new label. Save NotificationDecryptionContext
  with approved macID/deviceID, that binding, Mac signing public raw representation,
  and the client's agreement private key to NotificationDecryptionStore. No bearer
  or client signing private key belongs in that store.
- Install `await services.notificationClient(identity: revision, binding: binding)`
  on PushCenter. Give PushCenter the same NotificationDecryptionStore. Install the
  UNUserNotificationCenter delegate and route APNs success/failure callbacks into
  PushCenter. Register only after this activation sequence succeeds.
- On logout/replacement clear the decryption store and `push.install(nil)` before
  activating another identity. Clear old binding and delivered notifications so
  already-rendered private text does not remain visible. Old services deactivate.
- A scene consumes PushCenter.route after `isCurrent(route)`, forwards itemID to
  the TCA inbox navigation action and clears the route. Fresh inbox snapshots may
  be used for badge/stale notification cleanup; never use a disconnected snapshot.
- Notification mutations use the same CloudAppServices CommandGate, itemID and
  reserved UUID as interactive mutations. The background task expiration cancels
  submission; uncertain dispatch stays in the durable journal.

Registration uses signed relay POST /v1/push/register, then E2E executor POST
/v1/service/push-binding. The relay upsert is idempotent and serialized by
PushCenter; each settings update gets a fresh UUID reserved durably by the E2E
outbox. Reusing an old completed ID would suppress changing settings A → B → A.
The APNs token never reaches the executor. Source contracts were checked in the
service encrypted push worktrees (relay push.rs, Mac service.rs).

Quiet hours use encrypted `quiet_start`/`quiet_end` HH:mm fields together;
null/null disables them. The relay receives neither field. The executor uses
its local timezone, as the prior daemon implementation did.
Real APNs delivery and signing/provisioning remain unverified. The keychain group
must be provisioned before enabling encrypted rendering. Notifications older
than the signed ten-minute envelope validity fail closed on action/tap; a later
product flow may open the inbox without treating expired payload as authority.

Implemented platform wiring lives in ServiceNotificationRuntime, CloudPairingBootstrap,
and AppDelegate. CloudPairingOwner lifecycle hooks clean notification state before
logout while independently attempting pairing and Auth revocation even on errors.
Cold notification responses coalesce approved-session restoration before decoding.
Active scenes claim each route once; each scene keeps its own TCA Store.
Provisioning errors are visible; incomplete Keychain configuration is not silently
accepted. Signing/provisioning and actual APNs delivery remain release gates.

Versioned push preferences: PushPreferencesLedger fsyncs the full binding/quiet
snapshot and a monotonic per-pairing revision before any registration request.
Recreating the app or retrying unchanged settings retains that revision. Changing
or reverting settings advances it; the executor rejects delayed older writes.
An authenticated stale_push_preferences response permits one bounded retry above
current_revision, with the corrected local snapshot persisted before dispatch.
Corrupt/unwritable ledger state fails visibly and cannot silently reset counters.

## Multiple-context storage boundary

The binding defaults and shared decryption Keychain item now hold version-1
collections. Legacy records are decoded without writing. Binding adoption requires
the old relay/device/credential context to match the approved activation; the
credential revision includes the owner and pinned keys. Decryption-context adoption
also requires identical Mac/device IDs, binding and key bytes. Corrupt, locked or
future-version data cannot be overwritten as an empty collection. Duplicate labels
and changing the owner or keys behind an existing label fail closed.

The notification extension extracts the outer binding only as a lookup hint. It
loads exactly that record, then verifies the signed envelope, sender, recipient,
Mac route, expiration and inner binding before rendering any private text. It does
not fall back to another context. The app additionally requires the active binding
before routing an action; a retained inactive context cannot dispatch through the
current Mac's session. The shared item still contains only agreement private keys
and Mac public pins, never account tokens or client signing private keys.

Each store supports selective removal and explicit full cleanup. Synchronous
Keychain read/modify/write operations share a process mutex; the app is the only
writer and the extension reads atomic item snapshots. This is not an interprocess
writer lock. Migration supports existing data, not running an older app against a
new collection format.

Runtime activation remains single-session and its existing deactivate path still
clears both stores and notifications. No Mac selector is enabled by this change.
The next increment must separate selection from logout, keep registration and
sessions per approved pairing, coordinate selective revocation across stores, and
verify APNs fanout to two real Macs. Logout continues to attempt complete cleanup.
Scope records include relay and owner subject; the pairing model does not record
an OIDC issuer, so this does not establish support for arbitrary multiple issuers.
