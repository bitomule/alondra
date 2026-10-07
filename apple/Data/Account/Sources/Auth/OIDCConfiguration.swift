import Foundation

public struct OIDCConfiguration: Sendable {
    public enum RevocationScope: Sendable {
        case credential
        /// Requires a verified issuer contract: revoking a predecessor invalidates its successor.
        case offlineSession
    }
    public enum RefreshReplay: Sendable {
        /// Refreshing may consume the old token; a lost response leaves its successor unknown.
        case singleUse
        /// Requires a verified issuer contract: refreshing never invalidates the predecessor.
        case predecessorStaysValid
    }
    public let issuer: URL
    public let clientID: String
    public let redirectURI: URL
    public let scopes: [String]
    public let revocationScope: RevocationScope
    public let refreshReplay: RefreshReplay

    public init(issuer: URL, clientID: String, redirectURI: URL, scopes: [String],
                revocationScope: RevocationScope = .credential, refreshReplay: RefreshReplay = .singleUse) {
        self.issuer = issuer
        self.clientID = clientID
        self.redirectURI = redirectURI
        self.scopes = scopes
        self.revocationScope = revocationScope
        self.refreshReplay = refreshReplay
    }

    public static let alondra = OIDCConfiguration(
        issuer: URL(string: "https://alondra-auth.davidcollado.dev/realms/alondra")!,
        clientID: "alondra-ios",
        redirectURI: URL(string: "com.bitomule.alondra:/oauth2redirect/keycloak")!,
        scopes: ["openid", "profile", "offline_access"],
        // Keycloak 26.7.4, revokeRefreshToken=false. deploy/pi/smoke-refresh-revocation.py
        // checks that revoking a predecessor rejects every token it led to (passed 2026-09-26)
        // and that a refreshed predecessor still refreshes (added 2026-10-06, not yet run on
        // the Pi; Keycloak documents it under "Revoke Refresh Token"). Rerun on issuer drift.
        revocationScope: .offlineSession,
        refreshReplay: .predecessorStaysValid
    )
}

public enum AuthError: Error, Equatable, Sendable {
    case invalidDiscovery
    case invalidRedirect
    case stateMismatch
    case invalidToken
    /// The saved credential exists but cannot be read back. Only signing in again replaces it.
    case unreadableCredential
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
        case .pendingRevocation: String(localized: "Signing out didn’t finish. Finish signing out to continue.")
        case .rotationUncertain: String(localized: "Your session couldn’t be renewed. Sign out and sign in again to reconnect.")
        case .operationInProgress: String(localized: "Your account is busy with another step. Try again in a moment.")
        case .noSession: String(localized: "Sign in again to reconnect. Your pairing and saved work are kept.")
        case .oauth, .invalidDiscovery: String(localized: "The sign-in service isn’t working right now. Try again later.")
        case .httpStatus: String(localized: "The sign-in service isn’t answering. Alondra keeps trying.")
        case .keychain: String(localized: "This device couldn’t read its saved sign-in. Restart the app and try again.")
        case .unreadableCredential: String(localized: "Sign in again to reconnect. Your pairing and saved work are kept.")
        case .invalidRedirect, .stateMismatch, .invalidToken: String(localized: "Sign-in couldn’t be completed. Try again.")
        }
    }
}
