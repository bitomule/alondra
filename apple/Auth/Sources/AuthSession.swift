import CryptoKit
import Foundation
import Security

private struct Discovery: Decodable {
    let issuer: String
    let authorization_endpoint: URL
    let token_endpoint: URL
    let userinfo_endpoint: URL?
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
    private let configuration: OIDCConfiguration
    private let transport: any AuthHTTPTransport
    private let browser: any AuthBrowser
    private let storage: any RefreshTokenStorage
    private let now: @Sendable () -> Date
    private var accessToken: String?
    private var expiresAt: Date = .distantPast
    private var refreshTask: Task<String, Error>?
    private var epoch = 0

    public init(configuration: OIDCConfiguration = .alondra,
                transport: any AuthHTTPTransport = URLSessionAuthTransport(),
                browser: any AuthBrowser,
                storage: any RefreshTokenStorage = KeychainRefreshTokenStorage(),
                now: @escaping @Sendable () -> Date = { Date() }) {
        self.configuration = configuration
        self.transport = transport
        self.browser = browser
        self.storage = storage
        self.now = now
    }

    public func login() async throws {
        epoch += 1
        let startedAt = epoch
        refreshTask?.cancel()
        refreshTask = nil
        accessToken = nil
        expiresAt = .distantPast
        await browser.cancelAuthentication(generation: startedAt)
        try Task.checkCancellation()
        guard epoch == startedAt else { throw CancellationError() }
        try storage.delete()
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
        try Task.checkCancellation()
        guard epoch == startedAt else { throw CancellationError() }
        try validateIDToken(result.id_token, nonce: nonce)
        guard let refresh = result.refresh_token, !refresh.isEmpty else { throw AuthError.invalidToken }
        try storage.write(RefreshCredential(token: refresh))
        accessToken = result.access_token
        expiresAt = now().addingTimeInterval(TimeInterval(result.expires_in))
    }

    /// Returns a bearer token for the relay; caller must also sign the request with the paired device.
    public func bearerToken() async throws -> String {
        let credential = try activeCredential()
        if let accessToken, expiresAt.timeIntervalSince(now()) > 30 { return accessToken }
        if let refreshTask { return try await refreshTask.value }
        guard let saved = credential, !saved.token.isEmpty else { throw AuthError.noSession }
        let startedAt = epoch
        let task = Task { [self] in try await refresh(saved: saved.token, startedAt: startedAt) }
        refreshTask = task
        defer { if epoch == startedAt { refreshTask = nil } }
        return try await task.value
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
    public func hasSession() throws -> Bool { try activeCredential() != nil }

    private func activeCredential() throws -> RefreshCredential? {
        guard let credential = try storage.read() else { return nil }
        switch credential.status {
        case .active: return credential
        case .pendingRevocation: throw AuthError.pendingRevocation
        case .rotationUncertain: throw AuthError.rotationUncertain
        }
    }

    public func logout() async throws {
        epoch += 1
        let current = epoch
        refreshTask?.cancel()
        refreshTask = nil
        accessToken = nil
        expiresAt = .distantPast
        await browser.cancelAuthentication(generation: current)
        guard epoch == current else { throw CancellationError() }
        try storage.delete()
    }

    private func refresh(saved: String, startedAt: Int) async throws -> String {
        let document = try await discovery()
        let result = try await exchange(at: document.token_endpoint, fields: [
            "grant_type": "refresh_token", "refresh_token": saved, "client_id": configuration.clientID,
        ])
        try Task.checkCancellation()
        guard epoch == startedAt else { throw CancellationError() }
        if let rotated = result.refresh_token, !rotated.isEmpty { try storage.write(RefreshCredential(token: rotated)) }
        accessToken = result.access_token
        expiresAt = now().addingTimeInterval(TimeInterval(result.expires_in))
        return result.access_token
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
              document.userinfo_endpoint.map({ Self.allowedEndpoint($0, issuer: configuration.issuer) }) ?? true
        else { throw AuthError.invalidDiscovery }
        return document
    }

    private static func allowedEndpoint(_ endpoint: URL, issuer: URL) -> Bool {
        endpoint.scheme == "https" && endpoint.host == issuer.host && endpoint.port == issuer.port &&
        endpoint.user == nil && endpoint.password == nil && endpoint.fragment == nil &&
        endpoint.path.hasPrefix(issuer.path + "/")
    }

    private func exchange(at endpoint: URL, fields: [String: String]) async throws -> TokenResponse {
        var request = URLRequest(url: endpoint)
        request.httpMethod = "POST"
        request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.httpBody = fields.sorted { $0.key < $1.key }.map { "\($0.key.formEncoded)=\($0.value.formEncoded)" }.joined(separator: "&").data(using: .utf8)
        let response = try await transport.send(request)
        guard response.status == 200 else { throw AuthError.httpStatus(response.status) }
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
