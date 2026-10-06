import CryptoKit
import Foundation
import OSLog
import Security

private struct Discovery: Decodable {
    let issuer: String
    let authorization_endpoint: URL
    let token_endpoint: URL
    let userinfo_endpoint: URL?
    let revocation_endpoint: URL?
}

private struct UserInfo: Decodable {
    let sub: String
}

private struct TokenResponse: Decodable {
    let access_token: String
    let refresh_token: String?
    let id_token: String?
    let token_type: String
    let expires_in: Int
}

private struct IDClaims: Decodable {
    let iss: String
    let aud: Audience
    let nonce: String?
    let exp: Int
}

private enum Audience: Decodable {
    case one(String), many([String])
    init(from decoder: Decoder) throws {
        let box = try decoder.singleValueContainer()
        if let one = try? box.decode(String.self) { self = .one(one) }
        else { self = .many(try box.decode([String].self)) }
    }
    func contains(_ value: String) -> Bool {
        switch self { case .one(let item): item == value; case .many(let items): items.contains(value) }
    }
}

/// Owns credentials only. A relay request still requires the paired device's signature.
public actor AuthSession {
    /// Ends a grant that keeps the process alive while a refresh is on the wire.
    public typealias RefreshGrant = @Sendable () async -> @Sendable () async -> Void
    private static let log = Logger(subsystem: "com.bitomule.alondra", category: "auth")
    private let configuration: OIDCConfiguration
    private let transport: any AuthHTTPTransport
    private let browser: any AuthBrowser
    private let storage: any RefreshTokenStorage
    private let now: @Sendable () -> Date
    private let refreshGrant: RefreshGrant
    private var accessToken: String?
    private var expiresAt: Date = .distantPast
    private var refreshTask: Task<String, Error>?
    private var refreshTaskID: UUID?
    private var epoch = 0
    private var transitions = 0
    private var sessionSuspended = false
    // A successful issuer response can outlive a failed Keychain write. Keep its
    // successor until it is durable; logout must never fall back to the old token.
    private var unpersistedCredential: RefreshCredential?
    private var mutationLocked = false
    private var mutationWaiters: [AsyncStream<Void>.Continuation] = []

    private func reserveMutation() -> AsyncStream<Void> {
        let permit = AsyncStream<Void>.makeStream()
        if !mutationLocked {
            mutationLocked = true
            permit.continuation.yield(())
            permit.continuation.finish()
        } else { mutationWaiters.append(permit.continuation) }
        return permit.stream
    }

    private func awaitMutation(_ permit: AsyncStream<Void>) async {
        // The operation owns its reservation even when its caller is cancelled.
        // Await from a separate task so cancellation cannot skip a queued permit.
        await Task { for await _ in permit { return } }.value
    }

    private func releaseMutation() {
        if mutationWaiters.isEmpty { mutationLocked = false }
        else {
            let next = mutationWaiters.removeFirst()
            next.yield(()); next.finish()
        }
    }

    public init(configuration: OIDCConfiguration = .alondra,
                transport: any AuthHTTPTransport = URLSessionAuthTransport(),
                browser: any AuthBrowser,
                storage: any RefreshTokenStorage = KeychainRefreshTokenStorage(),
                now: @escaping @Sendable () -> Date = { Date() },
                refreshGrant: @escaping RefreshGrant = { { } }) {
        self.configuration = configuration
        self.transport = transport
        self.browser = browser
        self.storage = storage
        self.now = now
        self.refreshGrant = refreshGrant
    }

    public func login() async throws {
        let permit = reserveMutation()
        transitions += 1
        // A refresh may rotate even if its caller was cancelled. Finish persistence
        // before selecting the credential to revoke; never cancel that task here.
        await finishRefresh()
        await awaitMutation(permit)
        defer { releaseMutation(); transitions -= 1 }
        try Task.checkCancellation()
        try await performLogin()
    }

    private func performLogin() async throws {
        sessionSuspended = true
        epoch += 1
        let startedAt = epoch
        accessToken = nil
        expiresAt = .distantPast
        await browser.cancelAuthentication(generation: startedAt)
        try Task.checkCancellation()
        guard epoch == startedAt else { throw CancellationError() }
        try await revokeStoredCredential()
        let document = try await discovery()
        try Task.checkCancellation()
        guard epoch == startedAt else { throw CancellationError() }
        let state = try Self.randomURLSafe()
        let nonce = try Self.randomURLSafe()
        let verifier = try Self.randomURLSafe()
        let challenge = Data(SHA256.hash(data: Data(verifier.utf8))).base64URLEncoded
        var components = URLComponents(url: document.authorization_endpoint, resolvingAgainstBaseURL: false)!
        components.queryItems = [
            URLQueryItem(name: "response_type", value: "code"),
            URLQueryItem(name: "client_id", value: configuration.clientID),
            URLQueryItem(name: "redirect_uri", value: configuration.redirectURI.absoluteString),
            URLQueryItem(name: "scope", value: configuration.scopes.joined(separator: " ")),
            URLQueryItem(name: "state", value: state),
            URLQueryItem(name: "nonce", value: nonce),
            URLQueryItem(name: "code_challenge", value: challenge),
            URLQueryItem(name: "code_challenge_method", value: "S256"),
        ]
        let callback = try await browser.authenticate(url: components.url!, callbackScheme: configuration.redirectURI.scheme!, generation: startedAt)
        try Task.checkCancellation()
        guard epoch == startedAt else { throw CancellationError() }
        guard callback.scheme == configuration.redirectURI.scheme,
              callback.host == configuration.redirectURI.host,
              callback.path == configuration.redirectURI.path,
              callback.fragment == nil,
              let parts = URLComponents(url: callback, resolvingAgainstBaseURL: false),
              let values = parts.queryItems,
              values.filter({ $0.name == "state" }).count == 1,
              values.filter({ $0.name == "code" }).count == 1,
              values.allSatisfy({ $0.name == "state" || $0.name == "code" || $0.name == "session_state" || $0.name == "iss" })
        else { throw AuthError.invalidRedirect }
        guard values.first(where: { $0.name == "state" })?.value == state else { throw AuthError.stateMismatch }
        if let issuer = values.first(where: { $0.name == "iss" })?.value, issuer != configuration.issuer.absoluteString {
            throw AuthError.invalidRedirect
        }
        guard let code = values.first(where: { $0.name == "code" })?.value, !code.isEmpty else { throw AuthError.invalidRedirect }
        let result = try await exchange(at: document.token_endpoint, fields: [
            "grant_type": "authorization_code", "code": code,
            "client_id": configuration.clientID,
            "redirect_uri": configuration.redirectURI.absoluteString,
            "code_verifier": verifier,
        ])
        guard let refresh = result.refresh_token, !refresh.isEmpty else { throw AuthError.invalidToken }
        // Persist cleanup ownership before validating/accepting the issued session.
        // Cancellation must not leave an active credential restorable after restart.
        try persistIssuedCredential(RefreshCredential(token: refresh, status: .pendingRevocation))
        try validateIDToken(result.id_token, nonce: nonce)
        try Task.checkCancellation()
        guard epoch == startedAt else { throw CancellationError() }
        try persistIssuedCredential(RefreshCredential(token: refresh))
        accessToken = result.access_token
        expiresAt = now().addingTimeInterval(TimeInterval(result.expires_in))
        sessionSuspended = false
    }

    /// Returns a bearer token for the relay; caller must also sign the request with the paired device.
    public func bearerToken() async throws -> String {
        guard transitions == 0 else { throw AuthError.operationInProgress }
        guard !sessionSuspended else { throw AuthError.pendingRevocation }
        if let refreshTask { return try await refreshTask.value }
        let credential = try activeCredential()
        if let accessToken, expiresAt.timeIntervalSince(now()) > 30 { return accessToken }
        guard let saved = credential, !saved.token.isEmpty else { throw AuthError.noSession }
        let startedAt = epoch
        let permit = reserveMutation()
        let task = Task { [self] in
            try await refresh(saved: saved.token, uncertain: saved.status == .rotationUncertain, startedAt: startedAt, permit: permit)
        }
        let taskID = UUID()
        refreshTaskID = taskID
        refreshTask = task
        defer { if refreshTaskID == taskID { refreshTask = nil; refreshTaskID = nil } }
        return try await task.value
    }

    /// Bounded fragment transfers never start discovery or token renewal.
    /// Re-read persisted credential state so logout/revocation remains fail-closed.
    public func cachedBearerLease() throws -> @Sendable () async throws -> String {
        _ = try cachedBearerToken(generation: epoch)
        let generation = epoch
        return { try await self.cachedBearerToken(generation: generation) }
    }

    private func cachedBearerToken(generation: Int) throws -> String {
        guard epoch == generation else { throw AuthError.noSession }
        guard transitions == 0, refreshTask == nil else { throw AuthError.operationInProgress }
        guard !sessionSuspended else { throw AuthError.pendingRevocation }
        guard try activeCredential() != nil, let accessToken,
              expiresAt > now() else { throw AuthError.noSession }
        return accessToken
    }

    /// Reads the subject from the issuer's HTTPS UserInfo endpoint using the current bearer.
    /// This value is for pairing/account correlation; the relay still verifies the JWT itself.
    /// No claim from the locally decoded, unsigned ID-token payload is used as identity.
    public func verifiedSubject() async throws -> String {
        let startedAt = epoch
        let token = try await bearerToken()
        guard epoch == startedAt else { throw CancellationError() }
        let document = try await discovery()
        guard epoch == startedAt else { throw CancellationError() }
        guard let endpoint = document.userinfo_endpoint else { throw AuthError.invalidDiscovery }
        var request = URLRequest(url: endpoint)
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.cachePolicy = .reloadIgnoringLocalCacheData
        let response = try await transport.send(request)
        try Task.checkCancellation()
        guard epoch == startedAt else { throw CancellationError() }
        guard response.status == 200 else { throw AuthError.httpStatus(response.status) }
        let subject = try JSONDecoder().decode(UserInfo.self, from: response.body).sub
        guard !subject.isEmpty, subject.utf8.count <= 256,
              subject.utf8.allSatisfy({ $0 > 31 && $0 != 127 }) else { throw AuthError.invalidToken }
        return subject
    }

    /// UI may show a signed-in state without starting a refresh request.
    public func hasSession() throws -> Bool {
        guard !sessionSuspended else { throw AuthError.pendingRevocation }
        if refreshTask != nil { return true }
        return try activeCredential() != nil
    }

    public func requiresRevocation() throws -> Bool {
        if sessionSuspended { return true }
        if refreshTask != nil { return false }
        guard let credential = try storage.read() else { return false }
        switch credential.status {
        case .active: return false
        case .pendingRevocation: return true
        case .rotationUncertain: return !replaysUncertainRefresh
        }
    }

    private var replaysUncertainRefresh: Bool { configuration.refreshReplay == .predecessorStaysValid }

    private func activeCredential() throws -> RefreshCredential? {
        guard let credential = try storage.read() else { return nil }
        switch credential.status {
        case .active: return credential
        case .pendingRevocation: throw AuthError.pendingRevocation
        case .rotationUncertain:
            guard replaysUncertainRefresh else { throw AuthError.rotationUncertain }
            return credential
        }
    }

    public func logout() async throws {
        let permit = reserveMutation()
        transitions += 1
        sessionSuspended = true
        epoch += 1
        let current = epoch
        accessToken = nil
        expiresAt = .distantPast
        await browser.cancelAuthentication(generation: current)
        await finishRefresh()
        await awaitMutation(permit)
        defer { releaseMutation(); transitions -= 1 }
        // Complete the durable revocation transaction even if the presentation task
        // was cancelled. A failed operation leaves the credential blocked on disk.
        try await revokeStoredCredential()
        sessionSuspended = false
    }

    private func finishRefresh() async {
        let id = refreshTaskID
        if let refreshTask { _ = await refreshTask.result }
        if refreshTaskID == id { refreshTask = nil; refreshTaskID = nil }
    }

    private func persistIssuedCredential(_ credential: RefreshCredential) throws {
        unpersistedCredential = credential
        do {
            try storage.write(credential)
            unpersistedCredential = nil
        } catch {
            sessionSuspended = true
            accessToken = nil
            expiresAt = .distantPast
            throw error
        }
    }

    private func revokeStoredCredential() async throws {
        guard var credential = try unpersistedCredential ?? storage.read() else { return }
        if credential.status == .rotationUncertain {
            guard case .offlineSession = configuration.revocationScope else { throw AuthError.rotationUncertain }
            // Keep uncertainty durable until revocation succeeds. A failed request
            // must not downgrade it into an ordinary credential-only cleanup.
        } else {
            credential.status = .pendingRevocation
        }
        try storage.write(credential)
        unpersistedCredential = nil // The exact candidate is now durable for retry.
        accessToken = nil
        expiresAt = .distantPast
        let document = try await discovery()
        guard let endpoint = document.revocation_endpoint else { throw AuthError.invalidDiscovery }
        let response = try await post(at: endpoint, fields: [
            "client_id": configuration.clientID,
            "token": credential.token,
            "token_type_hint": "refresh_token",
        ])
        guard response.status == 200 else { throw oauthError(response) }
        try storage.delete()
    }

    private func refresh(saved: String, uncertain: Bool, startedAt: Int, permit: AsyncStream<Void>) async throws -> String {
        await awaitMutation(permit)
        defer { releaseMutation() }
        let endGrant = await refreshGrant()
        defer { Task { await endGrant() } }
        let document = try await discovery()
        // Persist the possibility of rotation before dispatch. A killed process or
        // missing response must not reuse a token whose successor is unknown, unless
        // the issuer keeps the predecessor valid (refreshReplay).
        try storage.write(RefreshCredential(token: saved, status: .rotationUncertain))
        Self.log.info("refresh started replaying=\(uncertain, privacy: .public)")
        let result: TokenResponse
        do {
            result = try await exchange(at: document.token_endpoint, fields: [
                "grant_type": "refresh_token", "refresh_token": saved, "client_id": configuration.clientID,
            ])
        } catch AuthError.oauth(let status, let code) {
            Self.log.error("refresh rejected status=\(status, privacy: .public) code=\(code, privacy: .public) replaying=\(uncertain, privacy: .public)")
            if code == "invalid_grant" {
                try await endRejectedSession(uncertain: uncertain)
                throw AuthError.noSession
            }
            // An explicit OAuth rejection did not issue a rotated credential.
            try storage.write(RefreshCredential(token: saved))
            throw AuthError.oauth(status: status, code: code)
        } catch let error as URLError {
            Self.log.error("refresh response lost urlError=\(error.code.rawValue, privacy: .public) replaying=\(uncertain, privacy: .public)")
            throw error
        } catch {
            // Preserve the transport/configuration error for the current caller;
            // the durable uncertain state prevents unsafe subsequent refreshes.
            Self.log.error("refresh failed \(String(describing: type(of: error)), privacy: .public) replaying=\(uncertain, privacy: .public)")
            throw error
        }
        let rotated = result.refresh_token.flatMap { $0.isEmpty ? nil : $0 } ?? saved
        try persistIssuedCredential(RefreshCredential(token: rotated))
        Self.log.info("refresh succeeded replaying=\(uncertain, privacy: .public)")
        // Persistence precedes cancellation/generation checks: logout needs this token.
        guard epoch == startedAt, transitions == 0 else { throw CancellationError() }
        accessToken = result.access_token
        expiresAt = now().addingTimeInterval(TimeInterval(result.expires_in))
        return result.access_token
    }

    /// The issuer refused the saved token. After a lost response its successor may still be
    /// live, so end the whole offline session; signing in again revokes it if this fails.
    private func endRejectedSession(uncertain: Bool) async throws {
        guard uncertain else { try storage.delete(); return }
        do {
            try await revokeStoredCredential()
            Self.log.info("rejected uncertain session revoked")
        } catch {
            Self.log.error("rejected uncertain session revocation failed \(String(describing: error), privacy: .public)")
        }
    }

    private func discovery() async throws -> Discovery {
        guard configuration.issuer.scheme == "https", configuration.issuer.user == nil,
              configuration.issuer.password == nil, configuration.issuer.query == nil,
              configuration.issuer.fragment == nil else { throw AuthError.invalidDiscovery }
        let url = configuration.issuer.appending(path: ".well-known/openid-configuration")
        let response = try await transport.send(URLRequest(url: url))
        guard response.status == 200 else { throw AuthError.httpStatus(response.status) }
        let document = try JSONDecoder().decode(Discovery.self, from: response.body)
        guard document.issuer == configuration.issuer.absoluteString,
              Self.allowedEndpoint(document.authorization_endpoint, issuer: configuration.issuer),
              Self.allowedEndpoint(document.token_endpoint, issuer: configuration.issuer),
              (document.userinfo_endpoint.map({ Self.allowedEndpoint($0, issuer: configuration.issuer) }) ?? true),
              (document.revocation_endpoint.map({ Self.allowedEndpoint($0, issuer: configuration.issuer) }) ?? true)
        else { throw AuthError.invalidDiscovery }
        return document
    }

    private static func allowedEndpoint(_ endpoint: URL, issuer: URL) -> Bool {
        endpoint.scheme == "https" && endpoint.host == issuer.host && endpoint.port == issuer.port &&
        endpoint.user == nil && endpoint.password == nil && endpoint.fragment == nil &&
        endpoint.path.hasPrefix(issuer.path + "/")
    }

    private func post(at endpoint: URL, fields: [String: String]) async throws -> AuthHTTPResponse {
        var request = URLRequest(url: endpoint)
        request.httpMethod = "POST"
        request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.httpBody = fields.sorted { $0.key < $1.key }.map { "\($0.key.formEncoded)=\($0.value.formEncoded)" }.joined(separator: "&").data(using: .utf8)
        return try await transport.send(request)
    }

    private func oauthError(_ response: AuthHTTPResponse) -> AuthError {
        struct Failure: Decodable { let error: String }
        if let failure = try? JSONDecoder().decode(Failure.self, from: response.body),
           !failure.error.isEmpty {
            return .oauth(status: response.status, code: failure.error)
        }
        return .httpStatus(response.status)
    }

    private func exchange(at endpoint: URL, fields: [String: String]) async throws -> TokenResponse {
        let response = try await post(at: endpoint, fields: fields)
        guard response.status == 200 else { throw oauthError(response) }
        let result = try JSONDecoder().decode(TokenResponse.self, from: response.body)
        guard result.token_type.lowercased() == "bearer", !result.access_token.isEmpty, result.expires_in > 0 else { throw AuthError.invalidToken }
        return result
    }

    private func validateIDToken(_ raw: String?, nonce: String) throws {
        guard let raw, let payload = raw.split(separator: ".").dropFirst().first,
              let data = Data(base64URL: String(payload)),
              let claims = try? JSONDecoder().decode(IDClaims.self, from: data),
              claims.iss == configuration.issuer.absoluteString,
              claims.aud.contains(configuration.clientID),
              claims.nonce == nonce,
              claims.exp > Int(now().timeIntervalSince1970)
        else { throw AuthError.invalidToken }
        // Claims are checked only for flow correlation. The relay verifies JWT signatures and audience.
        // Never use these unverified claims as an identity or authorization decision on device.
    }

    private static func randomURLSafe() throws -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else { throw AuthError.invalidToken }
        return Data(bytes).base64URLEncoded
    }
}

private extension Data {
    var base64URLEncoded: String { base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "") }
    init?(base64URL: String) {
        let padded = base64URL.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        self.init(base64Encoded: padded + String(repeating: "=", count: (4 - padded.count % 4) % 4))
    }
}

private extension String {
    var formEncoded: String {
        addingPercentEncoding(withAllowedCharacters: CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~"))!
    }
}
