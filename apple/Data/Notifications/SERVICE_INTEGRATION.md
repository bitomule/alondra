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
- At approved service activation derive a fresh PushBindingStore binding from
  relay URL, device ID and credential revision. Save NotificationDecryptionContext
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
