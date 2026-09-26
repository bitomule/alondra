import Foundation

public struct OIDCConfiguration: Sendable {
    public enum RevocationScope: Sendable {
        case credential
        /// Requires a verified issuer contract: revoking a predecessor invalidates its successor.
        case offlineSession
    }
    public let issuer: URL
    public let clientID: String
    public let redirectURI: URL
    public let scopes: [String]
    public let revocationScope: RevocationScope

    public init(issuer: URL, clientID: String, redirectURI: URL, scopes: [String],
                revocationScope: RevocationScope = .credential) {
        self.issuer = issuer
        self.clientID = clientID
        self.redirectURI = redirectURI
        self.scopes = scopes
        self.revocationScope = revocationScope
    }

    public static let alondra = OIDCConfiguration(
        issuer: URL(string: "https://alondra-auth.davidcollado.dev/realms/alondra")!,
        clientID: "alondra-ios",
        redirectURI: URL(string: "com.bitomule.alondra:/oauth2redirect/keycloak")!,
        scopes: ["openid", "profile", "offline_access"],
        // Keycloak 26.7.4, revokeRefreshToken=false: deploy/pi smoke verifies
        // predecessor revocation invalidates both tokens. Recheck on issuer drift.
        revocationScope: .offlineSession
    )
}

public enum AuthError: Error, Equatable, Sendable {
    case invalidDiscovery
    case invalidRedirect
    case stateMismatch
    case invalidToken
    case noSession
    case operationInProgress
    case oauth(status: Int, code: String)
    case pendingRevocation
    case rotationUncertain
    case httpStatus(Int)
    case keychain(Int32)
}

public struct AuthHTTPResponse: Sendable {
    public let status: Int
    public let body: Data
    public init(status: Int, body: Data) { self.status = status; self.body = body }
}

public protocol AuthHTTPTransport: Sendable {
    func send(_ request: URLRequest) async throws -> AuthHTTPResponse
}

public struct URLSessionAuthTransport: AuthHTTPTransport {
    private let session: URLSession
    public init() {
        let config = URLSessionConfiguration.ephemeral
        config.waitsForConnectivity = false
        self.session = URLSession(configuration: config, delegate: NoRedirectDelegate(), delegateQueue: nil)
    }
    public func send(_ request: URLRequest) async throws -> AuthHTTPResponse {
        let (data, response) = try await session.data(for: request)
        guard let response = response as? HTTPURLResponse else { throw AuthError.invalidToken }
        return AuthHTTPResponse(status: response.statusCode, body: data)
    }
}

/// Stateless delegate: URLSession may call it on any queue, and it never shares mutable state.
private final class NoRedirectDelegate: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

public protocol AuthBrowser: Sendable {
    func authenticate(url: URL, callbackScheme: String, generation: Int) async throws -> URL
    func cancelAuthentication(generation: Int) async
}

public protocol RefreshTokenStorage: Sendable {
    func read() throws -> RefreshCredential?
    func write(_ credential: RefreshCredential) throws
    func delete() throws
}


extension AuthError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .pendingRevocation: "Sign out is incomplete. Retry to revoke the saved session."
        case .rotationUncertain: "The refresh response was lost. Revoke this session from your account before reconnecting; local sign out cannot confirm revocation."
        case .operationInProgress: "An account operation is already in progress."
        case .noSession: "Sign in again to reconnect your saved workspace."
        case .oauth(_, let code) where code == "invalid_client": "The authentication client configuration was rejected."
        case .oauth: "The authentication service rejected the request."
        case .httpStatus(let status): "The authentication service returned HTTP \(status)."
        case .keychain(let status): "The secure credential store is unavailable (\(status))."
        case .invalidDiscovery: "The authentication service configuration is invalid."
        case .invalidRedirect, .stateMismatch: "The sign-in callback could not be verified."
        case .invalidToken: "The authentication credential could not be verified."
        }
    }
}
