import AlondraAccount
import Foundation
import Synchronization

/// Memory only; no Keychain or personal login. The lock protects the synchronous storage protocol.
final class SyntheticCredentialStorage: RefreshTokenStorage, Sendable {
    private let credential = Mutex<RefreshCredential?>(RefreshCredential(token: "synthetic-refresh"))
    func read() throws -> RefreshCredential? { credential.withLock { $0 } }
    func write(_ value: RefreshCredential) throws { credential.withLock { $0 = value } }
    func delete() throws { credential.withLock { $0 = nil } }
}

struct ForbiddenBrowser: AuthBrowser {
    func authenticate(url: URL, callbackScheme: String, generation: Int) async throws -> URL {
        throw AuthError.noSession
    }
    func cancelAuthentication(generation: Int) async {}
}

actor SyntheticAuthHTTP: AuthHTTPTransport {
    static let configuration = OIDCConfiguration(issuer: URL(string: "https://auth.fixture.invalid/realm")!,
        clientID: "synthetic-client", redirectURI: URL(string: "alondra-test:/callback")!, scopes: ["openid"])
    private var tokens = 0
    private var revocations = 0
    func observation() -> (tokens: Int, revocations: Int) { (tokens, revocations) }
    func send(_ request: URLRequest) async throws -> AuthHTTPResponse {
        guard let url = request.url, url.host == "auth.fixture.invalid", url.scheme == "https" else {
            throw AuthError.invalidDiscovery
        }
        if url.path == "/realm/.well-known/openid-configuration" {
            return AuthHTTPResponse(status: 200, body: Data(#"{"issuer":"https://auth.fixture.invalid/realm","authorization_endpoint":"https://auth.fixture.invalid/realm/auth","token_endpoint":"https://auth.fixture.invalid/realm/token","revocation_endpoint":"https://auth.fixture.invalid/realm/revoke"}"#.utf8))
        }
        if url.path == "/realm/token", request.httpMethod == "POST" {
            tokens += 1
            return AuthHTTPResponse(status: 200, body: Data(#"{"access_token":"synthetic-access","refresh_token":"synthetic-rotated","token_type":"Bearer","expires_in":300}"#.utf8))
        }
        if url.path == "/realm/revoke", request.httpMethod == "POST" {
            revocations += 1
            return AuthHTTPResponse(status: 200, body: Data())
        }
        throw AuthError.invalidDiscovery
    }
}
