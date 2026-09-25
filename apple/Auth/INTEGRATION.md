# iOS 0.1 OIDC integration contract

`AlondraAuth` is an iOS credential service. It contains no app state, reducer, view or
knowledge of the device signing key. The relay must verify both the bearer JWT
(`aud=alondra-relay`) and the signature of the **Mac-approved, paired** iPhone.
Login alone never enrolls a phone or grants access to content.

## Configuration

- Issuer: `https://alondra-auth.davidcollado.dev/realms/alondra`
- Client ID: `alondra-ios` (public client, no client secret)
- Redirect: `com.bitomule.alondra:/oauth2redirect/keycloak`
- Scopes: `openid profile`; the standard Keycloak authorization-code session is
  expected to return a refresh token. Do not add `offline_access` without a
  separate server/session policy decision.
- Authorization code + PKCE S256, random state and nonce. Discovery's issuer
  must equal the configured issuer; auth and token endpoints must be HTTPS on
  that host and under the realm path. Refresh tokens are stored in an
  after-first-unlock, this-device-only Keychain item, never in UserDefaults.

`AuthSession.bearerToken()` returns a token for the relay, but does not decide
whether a device is paired. The app must check pairing before creating a
remote session. OIDC ID-token claims are checked only to correlate the browser
round trip; **this module does not verify the ID-token signature and must not
use these claims as user identity or authorization**. The backend is the JWT
verification and authorization boundary. If client-side identity is ever
displayed or trusted, add JWKS signature validation first.

## H integration steps (single owner of app roots and APIClient)

1. Add `//apple/Auth:AlondraAuth` to `AlondraiOSEntry` in `apple/BUILD.bazel`.
   Add `CFBundleURLTypes` for scheme `com.bitomule.alondra` to `apple/iOS/Info.plist`.
   Ensure the registered Keycloak redirect URI is the exact URI above.
2. Construct one `AuthSession` in the iOS app composition root. Inject
   `SystemAuthBrowser` with a foreground scene's presentation anchor; do not
   construct it in a SwiftUI view. A Settings/Login reducer calls a narrow
   auth dependency for `login`, `logout` and current login state. The view only
   sends actions. Cancel the login effect on dismissal/account switch. A new
   login or logout cancels any older browser session; a late callback from that
   session cannot replace the newer account.
3. Extend `AlondraAPIClient` with an optional async bearer provider so
   `send(_:)` and `upload(_:)` obtain a fresh token before signing/transport.
   Set `Authorization: Bearer <token>` on the exact `URLRequest` to be sent.
   Preserve the existing device signature and canonical method/path/body;
   neither authorization header nor token changes the signed body. Do not
   attach a token to loopback health, initial pairing or other explicitly
   public endpoints. Do attach one to relay reads, writes, long polls, push
   registration and uploads. A 401 refresh/retry policy needs endpoint
   idempotency; do not automatically replay mutations.
4. Include login subject/session identity in the iOS sync-session key and
   invalidate snapshot, in-flight requests, command gate, push registration
   and pending navigation on login/logout/account switch. Pairing remains a
   separate authority; Mac signs `POST /v1/devices` after local approval.
5. Physical-device MAV gate: login redirects back to app, an unpaired login
   cannot read content, an approved pairing can read, logout clears the relay
   session and Keychain token, and account A's old responses cannot populate
   account B. Repeat APNs checks only after the extension's key-sharing and
   backend payload contract are ready.

The test target exercises the transport boundary with fake HTTP/browser and
memory storage. Production signing, redirect registration and physical auth
remain H/release gates.
