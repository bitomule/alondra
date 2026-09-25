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
The backend push binding must also be active for the paired iPhone.

D1b's `CommandGate` is now in main, but this headless path still calls
`NotificationAction.execute(using:)` once; it never automatically retries a
mutation after a timeout. Wire it to the final typed transport when the
backend idempotency contract is final. Persist unknown outcomes in the D1b
journal before treating notification actions as release ready.
The server may have committed an action before the client observes a timeout;
repeating it by hand can produce a duplicate. Resolve that unknown outcome
using the final D1b receipt/status endpoint before enabling retries.

The final MAV gate should deliver question options, permission, plan, and
report notifications on an iPhone, including two concurrent question pushes,
and verify the action buttons and one completion per selected action.
