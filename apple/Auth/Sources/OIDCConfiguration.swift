import Foundation

public struct OIDCConfiguration: Sendable {
    public let issuer: URL
    public let clientID: String
    public let redirectURI: URL
    public let scopes: [String]

    public init(issuer: URL, clientID: String, redirectURI: URL, scopes: [String]) {
        self.issuer = issuer
        self.clientID = clientID
        self.redirectURI = redirectURI
        self.scopes = scopes
    }

    public static let alondra = OIDCConfiguration(
        issuer: URL(string: "https://alondra-auth.davidcollado.dev/realms/alondra")!,
        clientID: "alondra-ios",
        redirectURI: URL(string: "com.bitomule.alondra:/oauth2redirect/keycloak")!,
        scopes: ["openid", "profile"]
    )
}

public enum AuthError: Error, Equatable, Sendable {
    case invalidDiscovery
    case invalidRedirect
    case stateMismatch
    case invalidToken
    case noSession
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
    func read() throws -> String?
    func write(_ token: String) throws
    func delete() throws
}
