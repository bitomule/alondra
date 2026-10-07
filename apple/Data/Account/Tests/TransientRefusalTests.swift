import AlondraAccount
import AlondraContract
import AlondraTransport
import Foundation
import Synchronization
import Testing

/// Answers discovery, then each token and userinfo request from its own script in order.
private actor ScriptedIssuer: AuthHTTPTransport {
    private var tokenReplies: [AuthHTTPResponse]
    private var userInfoReplies: [Int]
    private(set) var tokenCalls = 0
    private(set) var userInfoBearers: [String] = []

    init(tokens: [AuthHTTPResponse], userInfo: [Int]) {
        tokenReplies = tokens
        userInfoReplies = userInfo
    }

    func send(_ request: URLRequest) async throws -> AuthHTTPResponse {
        let path = request.url!.path
        if path.hasSuffix("openid-configuration") {
            return AuthHTTPResponse(status: 200, body: Data("""
            {"issuer":"https://alondra-auth.davidcollado.dev/realms/alondra","authorization_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/auth","token_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/token","revocation_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/revoke","userinfo_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/userinfo"}
            """.utf8))
        }
        if path.hasSuffix("/revoke") { return AuthHTTPResponse(status: 200, body: Data()) }
        if path.hasSuffix("/token") {
            tokenCalls += 1
            return tokenReplies.removeFirst()
        }
        userInfoBearers.append(request.value(forHTTPHeaderField: "Authorization") ?? "")
        let status = userInfoReplies.removeFirst()
        return AuthHTTPResponse(status: status, body: Data(status == 200 ? "{\"sub\":\"owner-1\"}".utf8 : "{}".utf8))
    }
}

private final class Storage: RefreshTokenStorage, @unchecked Sendable {
    private let value: Mutex<RefreshCredential?>
    init(_ token: String) { value = Mutex(RefreshCredential(token: token)) }
    func read() throws -> RefreshCredential? { value.withLock { $0 } }
    func write(_ credential: RefreshCredential) throws { value.withLock { $0 = credential } }
    func delete() throws { value.withLock { $0 = nil } }
}

private struct NoBrowser: AuthBrowser {
    func authenticate(url: URL, callbackScheme: String, generation: Int) async throws -> URL {
        Issue.record("A transient refusal must never open sign-in")
        throw CancellationError()
    }
    func cancelAuthentication(generation: Int) async {}
}

private final class Lines: Sendable {
    private let value = Mutex<[(String, Bool)]>([])
    var all: [(line: String, failed: Bool)] { value.withLock { $0.map { (line: $0.0, failed: $0.1) } } }
    var record: AuthSession.Diagnostics { { line, failed in self.value.withLock { $0.append((line, failed)) } } }
}

private func tokens(_ access: String, refresh: String) -> AuthHTTPResponse {
    AuthHTTPResponse(status: 200, body: try! JSONSerialization.data(withJSONObject: [
        "access_token": access, "refresh_token": refresh, "token_type": "Bearer", "expires_in": 300]))
}

private func oauth(_ status: Int, _ code: String) -> AuthHTTPResponse {
    AuthHTTPResponse(status: status, body: Data("{\"error\":\"\(code)\"}".utf8))
}

private func remote(_ error: any Error) -> ConnectionProblem {
    ConnectionClassifier.problem(for: error, local: false, networkOffline: false)
}

struct TransientRefusalTests {
    @Test(arguments: [401, 403])
    func userInfoRefusalRenewsOnceAndRecoversWithoutSignIn(status: Int) async throws {
        let issuer = ScriptedIssuer(tokens: [tokens("first", refresh: "r1"), tokens("second", refresh: "r2")],
                                    userInfo: [status, 200])
        let storage = Storage("r0")
        let session = AuthSession(transport: issuer, browser: NoBrowser(), storage: storage)

        let subject = try await session.verifiedSubject()

        #expect(subject == "owner-1")
        #expect(await issuer.userInfoBearers == ["Bearer first", "Bearer second"])
        #expect(await issuer.tokenCalls == 2)
        #expect(try storage.read() == RefreshCredential(token: "r2"))
    }

    @Test(arguments: [401, 403])
    func userInfoThatKeepsRefusingKeepsTheSessionAndRetries(status: Int) async throws {
        let issuer = ScriptedIssuer(tokens: [tokens("first", refresh: "r1"), tokens("second", refresh: "r2")],
                                    userInfo: [status, status])
        let storage = Storage("r0")
        let session = AuthSession(transport: issuer, browser: NoBrowser(), storage: storage)

        await #expect(throws: AuthError.httpStatus(status)) { try await session.verifiedSubject() }

        #expect(try storage.read() == RefreshCredential(token: "r2"))
        #expect(try await session.hasSession())
        let problem = remote(AuthError.httpStatus(status))
        #expect(problem == .accessNotConfirmed)
        #expect(!problem.needsAction)
    }

    @Test func userInfoRefusalEndsTheSessionOnlyWhenTheRenewalGetsInvalidGrant() async throws {
        let issuer = ScriptedIssuer(tokens: [tokens("first", refresh: "r1"), oauth(400, "invalid_grant")],
                                    userInfo: [401])
        let storage = Storage("r0")
        let session = AuthSession(transport: issuer, browser: NoBrowser(), storage: storage)

        await #expect(throws: AuthError.noSession) { try await session.verifiedSubject() }

        #expect(try storage.read() == nil)
        #expect(remote(AuthError.noSession) == .sessionExpired)
    }

    @Test func aRelayRefusalKeepsTheSession() {
        let problem = remote(RelayWireError.server(403))
        #expect(problem == .accessNotConfirmed)
        #expect(!problem.needsAction)
    }

    @Test func onlyTheMacsSealedRevocationSaysTheDeviceIsNoLongerPaired() {
        for unsealed in [RelayWireError.unauthorized as any Error, APIError.unauthorized] {
            #expect(remote(unsealed) == .accessNotConfirmed)
        }
        #expect(ConnectionClassifier.problem(for: DirectSessionError.deviceRevoked, local: false, networkOffline: false,
                                             macName: "Studio") == .deviceRevoked(macName: "Studio"))
    }

    @Test func aRelayRefusalRenewsTheTokenAndOnlyInvalidGrantEndsTheSession() async throws {
        let issuer = ScriptedIssuer(tokens: [tokens("first", refresh: "r1"), tokens("second", refresh: "r2"),
                                             oauth(400, "invalid_grant")], userInfo: [])
        let storage = Storage("r0")
        let clock = Mutex(Date(timeIntervalSince1970: 2_000_000_000))
        let session = AuthSession(transport: issuer, browser: NoBrowser(), storage: storage,
                                  now: { clock.withLock { $0 } })
        #expect(try await session.bearerToken() == "first")
        #expect(try await session.bearerToken() == "first")

        await session.accessTokenRefused()
        #expect(try await session.bearerToken() == "second")
        #expect(await issuer.tokenCalls == 2)
        #expect(try storage.read() == RefreshCredential(token: "r2"))

        clock.withLock { $0 = $0.addingTimeInterval(61) }
        await session.accessTokenRefused()
        await #expect(throws: AuthError.noSession) { try await session.bearerToken() }
        #expect(remote(AuthError.noSession) == .sessionExpired)
    }

    @Test func repeatedRelayRefusalsRenewAtMostOnceAMinute() async throws {
        let issuer = ScriptedIssuer(tokens: [tokens("first", refresh: "r1"), tokens("second", refresh: "r2"),
                                             tokens("third", refresh: "r3")], userInfo: [])
        let clock = Mutex(Date(timeIntervalSince1970: 2_000_000_000))
        let session = AuthSession(transport: issuer, browser: NoBrowser(), storage: Storage("r0"),
                                  now: { clock.withLock { $0 } })
        _ = try await session.bearerToken()
        await session.accessTokenRefused()
        #expect(try await session.bearerToken() == "second")
        for _ in 0..<5 {
            clock.withLock { $0 = $0.addingTimeInterval(10) }
            await session.accessTokenRefused()
            #expect(try await session.bearerToken() == "second")
        }
        #expect(await issuer.tokenCalls == 2)
        clock.withLock { $0 = $0.addingTimeInterval(11) }
        await session.accessTokenRefused()
        #expect(try await session.bearerToken() == "third")
        #expect(await issuer.tokenCalls == 3)
    }

    @Test func malformedServerAnswersKeepTheSession() {
        #expect(remote(AuthError.invalidToken) == .accessNotConfirmed)
        #expect(remote(URLError(.userAuthenticationRequired)) == .accessNotConfirmed)
        #expect(remote(AuthError.unreadableCredential) == .sessionExpired)
    }

    @Test func anOddTokenResponseNeitherSignsInNorRevokes() async throws {
        let odd = AuthHTTPResponse(status: 200, body: try JSONSerialization.data(withJSONObject: [
            "access_token": "a", "refresh_token": "r1", "token_type": "MAC", "expires_in": 300]))
        let issuer = ScriptedIssuer(tokens: [odd], userInfo: [])
        let storage = Storage("r0")
        let session = AuthSession(transport: issuer, browser: NoBrowser(), storage: storage)
        await #expect(throws: AuthError.invalidToken) { try await session.bearerToken() }
        #expect(try storage.read()?.token == "r0")
        #expect(remote(AuthError.invalidToken) == .accessNotConfirmed)
    }

    @Test func aUserInfoWithAnInvalidSubjectKeepsTheSession() async throws {
        struct BadSubject: AuthHTTPTransport {
            func send(_ request: URLRequest) async throws -> AuthHTTPResponse {
                if request.url!.path.hasSuffix("/userinfo") {
                    return AuthHTTPResponse(status: 200, body: Data(#"{"sub":""}"#.utf8))
                }
                return try await ScriptedIssuer(tokens: [tokens("first", refresh: "r1")], userInfo: []).send(request)
            }
        }
        let storage = Storage("r0")
        let session = AuthSession(transport: BadSubject(), browser: NoBrowser(), storage: storage)
        await #expect(throws: AuthError.invalidToken) { try await session.verifiedSubject() }
        #expect(try storage.read() == RefreshCredential(token: "r1"))
    }

    @Test func anUnreadableSavedCredentialAsksToSignInAndSigningInClearsIt() async throws {
        #expect(throws: AuthError.unreadableCredential) { try RefreshCredential.decodePersisted(Data("{broken".utf8)) }
        #expect(throws: AuthError.unreadableCredential) { try RefreshCredential.decodePersisted(Data()) }

        final class Unreadable: RefreshTokenStorage, @unchecked Sendable {
            let cleared = Mutex(false)
            func read() throws -> RefreshCredential? {
                if cleared.withLock({ $0 }) { return nil }
                throw AuthError.unreadableCredential
            }
            func write(_ credential: RefreshCredential) throws {}
            func delete() throws { cleared.withLock { $0 = true } }
        }
        struct Cancelling: AuthBrowser {
            func authenticate(url: URL, callbackScheme: String, generation: Int) async throws -> URL { throw CancellationError() }
            func cancelAuthentication(generation: Int) async {}
        }
        let storage = Unreadable()
        let session = AuthSession(transport: ScriptedIssuer(tokens: [], userInfo: []), browser: Cancelling(), storage: storage)
        await #expect(throws: AuthError.unreadableCredential) { try await session.bearerToken() }
        #expect(remote(AuthError.unreadableCredential) == .sessionExpired)
        await #expect(throws: CancellationError.self) { try await session.login() }
        #expect(storage.cleared.withLock { $0 })
    }

    @Test func onlyInvalidGrantAsksToSignInAgain() {
        #expect(remote(AuthError.oauth(status: 400, code: "invalid_grant")) == .sessionExpired)
        for code in ["invalid_client", "unauthorized_client", "invalid_request"] {
            #expect(remote(AuthError.oauth(status: 401, code: code)) == .accessNotConfirmed)
        }
        #expect(remote(AuthError.oauth(status: 503, code: "temporarily_unavailable")) == .serverUnreachable)
    }

    @Test func everyRenewalAndItsOutcomeIsLoggedWithoutSecrets() async throws {
        let lines = Lines()
        let issuer = ScriptedIssuer(tokens: [tokens("access-secret-1", refresh: "refresh-secret-1"),
                                             tokens("access-secret-2", refresh: "refresh-secret-2"),
                                             oauth(400, "invalid_grant")],
                                    userInfo: [401, 200])
        let clock = Mutex(Date(timeIntervalSince1970: 2_000_000_000))
        let session = AuthSession(transport: issuer, browser: NoBrowser(), storage: Storage("refresh-secret-0"),
                                  now: { clock.withLock { $0 } }, diagnostics: lines.record)

        _ = try await session.verifiedSubject()
        clock.withLock { $0 = $0.addingTimeInterval(3600) }
        await #expect(throws: AuthError.noSession) { try await session.bearerToken() }

        let text = lines.all.map(\.line)
        #expect(text == [
            "refresh started replaying=false",
            "refresh succeeded replaying=false rotated=true expiresIn=300",
            "userinfo refused status=401; renewing the access token",
            "refresh started replaying=false",
            "refresh succeeded replaying=false rotated=true expiresIn=300",
            "userinfo accepted the renewed access token",
            "refresh started replaying=false",
            "refresh rejected status=400 code=invalid_grant replaying=false",
            "session ended by the sign-in server; asking to sign in again",
        ])
        #expect(lines.all.filter(\.failed).count == 3)
        for line in text {
            #expect(!line.contains("secret"))
            #expect(!line.contains("owner-1"))
            #expect(!line.contains("http"))
        }
    }

    @Test func aRenewalThatNeverLeftTheDeviceIsLogged() async throws {
        struct Offline: AuthHTTPTransport {
            func send(_ request: URLRequest) async throws -> AuthHTTPResponse {
                if request.url!.path.hasSuffix("openid-configuration") {
                    return try await ScriptedIssuer(tokens: [], userInfo: []).send(request)
                }
                throw URLError(.notConnectedToInternet)
            }
        }
        let lines = Lines()
        let session = AuthSession(transport: Offline(), browser: NoBrowser(), storage: Storage("saved"),
                                  diagnostics: lines.record)
        _ = try? await session.bearerToken()
        #expect(lines.all.map(\.line) == [
            "refresh started replaying=false",
            "refresh not sent urlError=\(URLError.notConnectedToInternet.rawValue) replaying=false; session kept",
        ])
    }
}
