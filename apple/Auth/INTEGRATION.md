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

`AuthSession.verifiedSubject()` obtains `sub` from the issuer's HTTPS UserInfo
endpoint using the current bearer token. Discovery requires that endpoint to
stay on the issuer host and under its realm path; the transport refuses
redirects. The method discards a result that arrives after logout or account
switch. Pass this value to pairing as `owner_sub` for correlation. The relay
still verifies the bearer token and its subject independently.

## H integration steps (single owner of app roots and APIClient)

1. Add `//apple/Auth:AlondraAuth` and `//apple/AuthPresentation:AlondraAuthPresentation`
   to app entry targets in `apple/BUILD.bazel`. AuthPresentation owns the system
   web-auth browser; Auth keeps the session and headless browser protocol.
   Add `CFBundleURLTypes` for scheme `com.bitomule.alondra` to `apple/iOS/Info.plist`.
   Ensure the registered Keycloak redirect URI is the exact URI above.
2. Construct one `AuthSession` in the iOS app composition root. Inject
   `SystemAuthBrowser` with a foreground scene's presentation anchor; do not
   construct it in a SwiftUI view. A Settings/Login reducer calls a narrow
   auth dependency for `login`, `logout` and current login state. The view only
   sends actions. Cancel the login effect on dismissal/account switch. A new
   login or logout cancels any older browser session; a late callback from that
   session cannot replace the newer account.
3. Construct `AlondraRelayClient` with an async bearer provider from this
   AuthSession. The relay client enforces HTTPS and signs its separate
   `x-device-*` challenge. Keep `AlondraAPIClient` for the local daemon only;
   it must not carry an OIDC bearer to HTTP loopback/LAN URLs. Do not
   automatically replay a mutation after 401 or network ambiguity.
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
