# D3 notification integration

`AlondraNotificationActions` is a headless action boundary. `PushCenter` obtains a
server-scoped signer through D2's `DevicePairing.signer(for:)`, owns the iOS
background-task deadline, and passes the signed client to the action. The
notification extension imports only the pure `AlondraNotificationPayload`
module. It has no keychain, client, SwiftUI, TCA, or application state access.

The service extension is the only writer of `UNNotificationCategory` data. It
registers fixed categories alongside dynamic question categories under an
interprocess file lock in its own container. The app no longer rewrites the
categories at launch. Every APNs payload that needs a category **must** include
`aps.mutable-content: 1`, otherwise the extension cannot register its category
before the first delivery. Question/permission/plan payloads already do. The
daemon's report payload (`ALONDRA_READ`) gains that field in backend PR #130,
commit `2c899d8`. Merge and deploy that backend change before this iOS app version.
The backend push binding must also be active for the paired iPhone. Registration
now sends a canonical 43-character base64url `push_binding` generated from 32
random bytes and stored for the current server URL, device ID, and credential.
Changing any of those rotates the binding. APNs must echo it inside `alondra`;
payloads without a matching binding cannot open an item or send an action.
Dynamic question categories include the binding in their IDs. The extension
keeps at most 20 recognized dynamic categories globally, ordered by a small
ledger in its own container under the category lock. It cannot infer the
current pairing from the binding of a delayed APNs message, so a late A push
does not purge recently registered B categories. Unrecognized category IDs,
including other `ALONDRA_Q_` prefixes, are preserved. Delivered-notification
cleanup only touches the current binding. Send-failure notices
carry no action category or original payload.

D1b's app-owned `CommandGate` now reserves the command in its durable journal
before `NotificationAction.execute(using:)` dispatches. Push actions never retry
automatically after a timeout. The pairing credential revision must match the
active FleetStore scope, and the binding is checked again just before dispatch;
an unknown outcome leaves an in-app warning and an informational local notice.
The notification service extension still holds no signing key or journal access.
The server may have committed an action before the client observes a timeout;
repeating it by hand can produce a duplicate. Resolve that unknown outcome
using the final D1b receipt/status endpoint before enabling retries.

The final MAV gate should deliver question options, permission, plan, and
report notifications on an iPhone, including two concurrent question pushes,
and verify the action buttons and one completion per selected action.
