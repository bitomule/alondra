import Foundation
import CryptoKit
import XCTest
import AlondraAuth

private actor FixtureTransport: AuthHTTPTransport {
    var requests: [URLRequest] = []
    var tokenResponse: @Sendable (URLRequest) throws -> AuthHTTPResponse
    let revocationResponse: @Sendable (URLRequest) -> AuthHTTPResponse
    init(revocationResponse: @escaping @Sendable (URLRequest) -> AuthHTTPResponse = { _ in AuthHTTPResponse(status: 200, body: Data()) },
         tokenResponse: @escaping @Sendable (URLRequest) throws -> AuthHTTPResponse) {
        self.tokenResponse = tokenResponse
        self.revocationResponse = revocationResponse
    }
    func send(_ request: URLRequest) async throws -> AuthHTTPResponse {
        requests.append(request)
        if request.url!.path.hasSuffix("openid-configuration") {
            return AuthHTTPResponse(status: 200, body: Data("""
            {"issuer":"https://alondra-auth.davidcollado.dev/realms/alondra","authorization_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/auth","token_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/token","revocation_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/revoke","userinfo_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/userinfo"}
            """.utf8))
        }
        if request.url!.path.hasSuffix("/revoke") { return revocationResponse(request) }
        return try tokenResponse(request)
    }
    func tokenRequests() -> [URLRequest] { requests.filter { $0.url!.path.hasSuffix("/token") } }
    func revocations() -> [URLRequest] { requests.filter { $0.url!.path.hasSuffix("/revoke") } }
}

private actor BlockingRefreshTransport: AuthHTTPTransport {
    private var startWaiter: CheckedContinuation<Void, Never>?
    private var responseWaiter: CheckedContinuation<AuthHTTPResponse, Never>?
    private var started = false
    private var postCount = 0
    private var revoked: [URLRequest] = []
    func send(_ request: URLRequest) async throws -> AuthHTTPResponse {
        if request.httpMethod != "POST" {
            return AuthHTTPResponse(status: 200, body: Data("""
            {"issuer":"https://alondra-auth.davidcollado.dev/realms/alondra","authorization_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/auth","token_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/token","revocation_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/revoke"}
            """.utf8))
        }
        if request.url!.path.hasSuffix("/revoke") {
            revoked.append(request)
            return AuthHTTPResponse(status: 200, body: Data())
        }
        postCount += 1
        started = true
        startWaiter?.resume()
        startWaiter = nil
        return await withCheckedContinuation { responseWaiter = $0 }
    }
    func waitForPost() async {
        if started { return }
        await withCheckedContinuation { startWaiter = $0 }
    }
    func release() {
        responseWaiter?.resume(returning: AuthSessionTests.tokens(access: "late", refresh: "rotated", idToken: nil))
        responseWaiter = nil
    }
    func count() -> Int { postCount }
    func revocations() -> [URLRequest] { revoked }
}

private struct InvalidDiscoveryTransport: AuthHTTPTransport {
    func send(_ request: URLRequest) async throws -> AuthHTTPResponse {
        AuthHTTPResponse(status: 200, body: Data("""
        {"issuer":"https://attacker.example/realms/alondra","authorization_endpoint":"https://attacker.example/auth","token_endpoint":"https://attacker.example/token"}
        """.utf8))
    }
}

private actor UserInfoTransport: AuthHTTPTransport {
    private let holdUserInfo: Bool
    private var userInfoStarted = false
    private var startWaiter: CheckedContinuation<Void, Never>?
    private var responseWaiter: CheckedContinuation<Void, Never>?
    private var userInfoAuthorization: String?

    init(holdUserInfo: Bool = false) { self.holdUserInfo = holdUserInfo }

    func send(_ request: URLRequest) async throws -> AuthHTTPResponse {
        if request.url!.path.hasSuffix("openid-configuration") {
            return AuthHTTPResponse(status: 200, body: Data("""
            {"issuer":"https://alondra-auth.davidcollado.dev/realms/alondra","authorization_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/auth","token_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/token","revocation_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/revoke","userinfo_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/userinfo"}
            """.utf8))
        }
        if request.url!.path.hasSuffix("/revoke") { return AuthHTTPResponse(status: 200, body: Data()) }
        if request.httpMethod == "POST" {
            return AuthSessionTests.tokens(access: "access-for-userinfo", refresh: "rotated", idToken: nil)
        }
        userInfoAuthorization = request.value(forHTTPHeaderField: "Authorization")
        userInfoStarted = true
        startWaiter?.resume()
        startWaiter = nil
        if holdUserInfo { await withCheckedContinuation { responseWaiter = $0 } }
        return AuthHTTPResponse(status: 200, body: Data("{\"sub\":\"owner-1\"}".utf8))
    }

    func waitForUserInfo() async {
        if userInfoStarted { return }
        await withCheckedContinuation { startWaiter = $0 }
    }

    func releaseUserInfo() {
        responseWaiter?.resume()
        responseWaiter = nil
    }

    func authorization() -> String? { userInfoAuthorization }
}

private struct OffIssuerUserInfoTransport: AuthHTTPTransport {
    func send(_ request: URLRequest) async throws -> AuthHTTPResponse {
        AuthHTTPResponse(status: 200, body: Data("""
        {"issuer":"https://alondra-auth.davidcollado.dev/realms/alondra","authorization_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/auth","token_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/token","revocation_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/revoke","userinfo_endpoint":"https://attacker.example/userinfo"}
        """.utf8))
    }
}

private struct FixtureBrowser: AuthBrowser {
    let callback: @Sendable (URL) -> URL
    func authenticate(url: URL, callbackScheme: String, generation: Int) async throws -> URL { callback(url) }
    func cancelAuthentication(generation: Int) async {}
}

private actor ControlledBrowser: AuthBrowser {
    private struct Pending {
        let id: Int
        let continuation: CheckedContinuation<URL, Error>
    }
    private var active: Pending?
    private var starts: [URL] = []
    private var startWaiter: CheckedContinuation<Void, Never>?
    private var newestGeneration = 0

    func authenticate(url: URL, callbackScheme: String, generation: Int) async throws -> URL {
        guard generation >= newestGeneration else { throw CancellationError() }
        newestGeneration = generation
        guard active == nil else { throw AuthError.invalidRedirect }
        let id = starts.count + 1
        starts.append(url)
        startWaiter?.resume()
        startWaiter = nil
        return try await withCheckedThrowingContinuation { active = Pending(id: id, continuation: $0) }
    }

    func cancelAuthentication(generation: Int) async {
        guard generation >= newestGeneration else { return }
        newestGeneration = generation
        guard let pending = active else { return }
        active = nil
        pending.continuation.resume(throwing: CancellationError())
    }

    func waitForStart(_ count: Int) async -> URL {
        while starts.count < count {
            await withCheckedContinuation { startWaiter = $0 }
        }
        return starts[count - 1]
    }

    func finish(id: Int, callback: URL) {
        guard let pending = active, pending.id == id else { return }
        active = nil
        pending.continuation.resume(returning: callback)
    }
}

private actor DelayedCancelBrowser: AuthBrowser {
    private let evidence: FlowEvidence
    private var cancelWaiter: CheckedContinuation<Void, Never>?
    private var startedWaiter: CheckedContinuation<Void, Never>?
    private var firstCancelStarted = false

    init(evidence: FlowEvidence) { self.evidence = evidence }

    func authenticate(url: URL, callbackScheme: String, generation: Int) async throws -> URL {
        let query = URLComponents(url: url, resolvingAgainstBaseURL: false)!.queryItems!
        let state = query.first { $0.name == "state" }!.value!
        let nonce = query.first { $0.name == "nonce" }!.value!
        evidence.record(nonce: nonce, challenge: "")
        return URL(string: "com.bitomule.alondra:/oauth2redirect/keycloak?code=account-b&state=\(state)")!
    }

    func cancelAuthentication(generation: Int) async {
        guard generation == 1 else { return }
        firstCancelStarted = true
        startedWaiter?.resume()
        startedWaiter = nil
        await withCheckedContinuation { cancelWaiter = $0 }
    }

    func waitForFirstCancel() async {
        if firstCancelStarted { return }
        await withCheckedContinuation { startedWaiter = $0 }
    }

    func releaseFirstCancel() {
        cancelWaiter?.resume()
        cancelWaiter = nil
    }
}

private final class MemoryStorage: RefreshTokenStorage, @unchecked Sendable {
    private let lock = NSLock()
    private var token: RefreshCredential?
    private let refusesWrites: Bool
    init(_ token: String? = nil, refusesWrites: Bool = false) {
        self.token = token.map { RefreshCredential(token: $0) }
        self.refusesWrites = refusesWrites
    }
    func read() throws -> RefreshCredential? { lock.lock(); defer { lock.unlock() }; return token }
    func write(_ token: RefreshCredential) throws { lock.lock(); defer { lock.unlock() }; if refusesWrites { throw AuthError.keychain(-25293) }; self.token = token }
    func delete() throws { lock.lock(); defer { lock.unlock() }; token = nil }
}

private final class FlowEvidence: @unchecked Sendable {
    private let lock = NSLock()
    private var nonce = ""
    private var challenge = ""
    func record(nonce: String, challenge: String) { lock.lock(); defer { lock.unlock() }; self.nonce = nonce; self.challenge = challenge }
    func values() -> (String, String) { lock.lock(); defer { lock.unlock() }; return (nonce, challenge) }
}

final class AuthSessionTests: XCTestCase {
    func testVerifiedSubjectUsesPinnedUserInfoAndBearer() async throws {
        let transport = UserInfoTransport()
        let browser = FixtureBrowser { _ in XCTFail("Browser should not open"); return URL(string: "https://invalid.example")! }
        let session = AuthSession(transport: transport, browser: browser, storage: MemoryStorage("refresh"))

        let subject = try await session.verifiedSubject()
        let authorization = await transport.authorization()
        XCTAssertEqual(subject, "owner-1")
        XCTAssertEqual(authorization, "Bearer access-for-userinfo")
    }

    func testLogoutWhileUserInfoPendingCannotReturnOldSubject() async throws {
        let transport = UserInfoTransport(holdUserInfo: true)
        let browser = FixtureBrowser { _ in XCTFail("Browser should not open"); return URL(string: "https://invalid.example")! }
        let session = AuthSession(transport: transport, browser: browser, storage: MemoryStorage("refresh"))
        let pending = Task { try await session.verifiedSubject() }
        await transport.waitForUserInfo()
        try await session.logout()
        await transport.releaseUserInfo()
        do { _ = try await pending.value; XCTFail("Old account subject must not escape") }
        catch is CancellationError { }
    }

    func testUserInfoEndpointMustStayUnderIssuer() async throws {
        let browser = FixtureBrowser { _ in XCTFail("Browser should not open"); return URL(string: "https://invalid.example")! }
        let session = AuthSession(transport: OffIssuerUserInfoTransport(), browser: browser,
                                  storage: MemoryStorage("refresh"))
        do { _ = try await session.verifiedSubject(); XCTFail("Expected rejected discovery") }
        catch AuthError.invalidDiscovery { }
    }

    func testLoginUsesPKCEAndPersistsRefreshOnlyAfterValidCallback() async throws {
        let storage = MemoryStorage()
        let evidence = FlowEvidence()
        let transport = FixtureTransport { request in
            let form = Self.form(request)
            XCTAssertEqual(form["grant_type"], "authorization_code")
            XCTAssertEqual(form["client_id"], "alondra-ios")
            XCTAssertEqual(form["redirect_uri"], "com.bitomule.alondra:/oauth2redirect/keycloak")
            XCTAssertEqual(form["code"], "auth-code")
            let idToken = Self.idToken(nonce: evidence.values().0)
            return Self.tokens(access: "access-1", refresh: "refresh-1", idToken: idToken)
        }
        let browser = FixtureBrowser { url in
            let query = URLComponents(url: url, resolvingAgainstBaseURL: false)!.queryItems!
            let values = Dictionary(uniqueKeysWithValues: query.map { ($0.name, $0.value!) })
            XCTAssertEqual(values["code_challenge_method"], "S256")
            XCTAssertEqual(values["scope"], "openid profile offline_access")
            evidence.record(nonce: values["nonce"]!, challenge: values["code_challenge"]!)
            return URL(string: "com.bitomule.alondra:/oauth2redirect/keycloak?code=auth-code&state=\(values["state"]!)")!
        }
        let session = AuthSession(transport: transport, browser: browser, storage: storage,
                                  now: { Date(timeIntervalSince1970: 2_000_000_000) })
        try await session.login()
        let signedIn = try await session.hasSession()
        XCTAssertTrue(signedIn)
        let bearer = try await session.bearerToken()
        XCTAssertEqual(bearer, "access-1")
        XCTAssertEqual(try storage.read()?.token, "refresh-1")
        let posts = await transport.tokenRequests()
        XCTAssertEqual(posts.count, 1)
        let form = Self.form(posts[0])
        let challenge = Data(SHA256.hash(data: Data(form["code_verifier"]!.utf8))).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        XCTAssertEqual(challenge, evidence.values().1)
    }

    func testStateMismatchNeverExchangesCode() async throws {
        let transport = FixtureTransport { _ in XCTFail("Must not exchange"); return Self.tokens(access: "x", refresh: "y", idToken: "z") }
        let browser = FixtureBrowser { _ in URL(string: "com.bitomule.alondra:/oauth2redirect/keycloak?code=auth-code&state=wrong")! }
        let session = AuthSession(transport: transport, browser: browser, storage: MemoryStorage())
        do { try await session.login(); XCTFail("Expected state rejection") }
        catch AuthError.stateMismatch { }
        let posts = await transport.tokenRequests()
        XCTAssertEqual(posts.count, 0)
    }

    func testDiscoveryCannotMoveAuthorizationToAnotherIssuer() async throws {
        let browser = FixtureBrowser { _ in XCTFail("Browser must not open"); return URL(string: "https://invalid.example")! }
        let session = AuthSession(transport: InvalidDiscoveryTransport(), browser: browser, storage: MemoryStorage())
        do { try await session.login(); XCTFail("Expected rejected issuer") }
        catch AuthError.invalidDiscovery { }
    }

    func testCallbackPathCannotBeSubstituted() async throws {
        let transport = FixtureTransport { _ in XCTFail("Must not exchange"); return Self.tokens(access: "x", refresh: "y", idToken: "z") }
        let browser = FixtureBrowser { url in
            let state = URLComponents(url: url, resolvingAgainstBaseURL: false)!.queryItems!.first { $0.name == "state" }!.value!
            return URL(string: "com.bitomule.alondra:/other/path?code=auth-code&state=\(state)")!
        }
        let session = AuthSession(transport: transport, browser: browser, storage: MemoryStorage())
        do { try await session.login(); XCTFail("Expected rejected redirect") }
        catch AuthError.invalidRedirect { }
        let posts = await transport.tokenRequests()
        XCTAssertEqual(posts.count, 0)
    }

    func testRefreshRotationAndLogoutDropCredentials() async throws {
        let storage = MemoryStorage("old-refresh")
        let transport = FixtureTransport { request in
            XCTAssertEqual(Self.form(request)["refresh_token"], "old-refresh")
            return Self.tokens(access: "new-access", refresh: "new-refresh", idToken: nil)
        }
        let browser = FixtureBrowser { _ in XCTFail("Browser should not open"); return URL(string: "https://invalid.example")! }
        let session = AuthSession(transport: transport, browser: browser, storage: storage)
        let bearer = try await session.bearerToken()
        XCTAssertEqual(bearer, "new-access")
        XCTAssertEqual(try storage.read()?.token, "new-refresh")
        try await session.logout()
        let signedIn = try await session.hasSession()
        XCTAssertFalse(signedIn)
        do { _ = try await session.bearerToken(); XCTFail("Expected no session") }
        catch AuthError.noSession { }
    }

    func testLogoutDuringRefreshCannotRestoreOldAccount() async throws {
        let storage = MemoryStorage("old-refresh")
        let transport = BlockingRefreshTransport()
        let browser = DelayedCancelBrowser(evidence: FlowEvidence())
        let session = AuthSession(transport: transport, browser: browser, storage: storage)
        let pending = Task { try await session.bearerToken() }
        await transport.waitForPost()
        let logout = Task { try await session.logout() }
        await browser.waitForFirstCancel()
        await browser.releaseFirstCancel()
        await transport.release()
        try await logout.value
        do { _ = try await pending.value; XCTFail("Old refresh must fail") }
        catch is CancellationError { }
        XCTAssertNil(try storage.read())
        let count = await transport.count()
        XCTAssertEqual(count, 1)
        let revocations = await transport.revocations()
        XCTAssertEqual(revocations.count, 1)
        XCTAssertEqual(Self.form(try XCTUnwrap(revocations.first))["token"], "rotated")
    }

    func testSecondLoginWaitsForFirstAndRevokesBeforeNewAuthorization() async throws {
        let storage = MemoryStorage()
        let evidence = FlowEvidence()
        let transport = FixtureTransport { _ in Self.tokens(access: "account-b", refresh: "refresh-b", idToken: Self.idToken(nonce: evidence.values().0)) }
        let browser = ControlledBrowser()
        let session = AuthSession(transport: transport, browser: browser, storage: storage,
                                  now: { Date(timeIntervalSince1970: 2_000_000_000) })

        let first = Task { try await session.login() }
        let firstURL = await browser.waitForStart(1)
        let second = Task { try await session.login() }
        let firstQuery = URLComponents(url: firstURL, resolvingAgainstBaseURL: false)!.queryItems!
        let firstState = firstQuery.first { $0.name == "state" }!.value!
        evidence.record(nonce: firstQuery.first { $0.name == "nonce" }!.value!, challenge: "")
        await browser.finish(id: 1, callback: URL(string: "com.bitomule.alondra:/oauth2redirect/keycloak?code=a&state=\(firstState)")!)
        try await first.value
        let secondURL = await browser.waitForStart(2)
        let revocations = await transport.revocations()
        XCTAssertEqual(revocations.count, 1, "Old session must be revoked before second browser starts")
        await browser.finish(id: 1, callback: URL(string: "com.bitomule.alondra:/oauth2redirect/keycloak?code=late&state=\(firstState)")!)
        let secondQuery = URLComponents(url: secondURL, resolvingAgainstBaseURL: false)!.queryItems!
        let secondState = secondQuery.first { $0.name == "state" }!.value!
        evidence.record(nonce: secondQuery.first { $0.name == "nonce" }!.value!, challenge: "")
        await browser.finish(id: 2, callback: URL(string: "com.bitomule.alondra:/oauth2redirect/keycloak?code=b&state=\(secondState)")!)

        try await second.value
        let bearer = try await session.bearerToken()
        XCTAssertEqual(bearer, "account-b")
        XCTAssertEqual(try storage.read()?.token, "refresh-b")
        let posts = await transport.tokenRequests()
        XCTAssertEqual(posts.count, 2)
        XCTAssertEqual(Self.form(posts[1])["code"], "b")
    }

    func testDelayedLogoutCannotDeleteCredentialsFromNewerLogin() async throws {
        let storage = MemoryStorage("account-a-refresh")
        let evidence = FlowEvidence()
        let browser = DelayedCancelBrowser(evidence: evidence)
        let transport = FixtureTransport { _ in
            Self.tokens(access: "account-b-access", refresh: "account-b-refresh", idToken: Self.idToken(nonce: evidence.values().0))
        }
        let session = AuthSession(transport: transport, browser: browser, storage: storage,
                                  now: { Date(timeIntervalSince1970: 2_000_000_000) })

        let oldLogout = Task { try await session.logout() }
        await browser.waitForFirstCancel()
        let login = Task { try await session.login() }
        await browser.releaseFirstCancel()
        try await oldLogout.value
        try await login.value
        let revocations = await transport.revocations()
        XCTAssertEqual(revocations.count, 1)
        XCTAssertEqual(Self.form(try XCTUnwrap(revocations.first))["token"], "account-a-refresh")
        XCTAssertEqual(try storage.read()?.token, "account-b-refresh")
        let token = try await session.bearerToken()
        XCTAssertEqual(token, "account-b-access")
    }

    func testFailedLogoutSurvivesRestartAndRetryDeletesOnlyAfterRevocation() async throws {
        let storage = MemoryStorage("refresh-to-revoke")
        let browser = FixtureBrowser { _ in XCTFail("Revocation must not open a browser"); return URL(string: "https://invalid.example")! }
        let failed = FixtureTransport(revocationResponse: { _ in AuthHTTPResponse(status: 503, body: Data()) }) { _ in
            XCTFail("Pending logout must not refresh"); throw AuthError.invalidToken
        }
        let session = AuthSession(transport: failed, browser: browser, storage: storage)
        do { try await session.logout(); XCTFail("Cannot report successful logout") }
        catch AuthError.httpStatus(503) { }
        XCTAssertEqual(try storage.read(), RefreshCredential(token: "refresh-to-revoke", status: .pendingRevocation))
        let recovered = FixtureTransport(revocationResponse: { request in
            XCTAssertEqual(Self.form(request)["token"], "refresh-to-revoke")
            XCTAssertEqual(Self.form(request)["token_type_hint"], "refresh_token")
            XCTAssertEqual(Self.form(request)["client_id"], "alondra-ios")
            XCTAssertNil(Self.form(request)["client_secret"])
            return AuthHTTPResponse(status: 200, body: Data())
        }) { _ in XCTFail("No token request during pending logout"); throw AuthError.invalidToken }
        let restarted = AuthSession(transport: recovered, browser: browser, storage: storage)
        do { _ = try await restarted.bearerToken(); XCTFail("Pending token must remain blocked") }
        catch AuthError.pendingRevocation { }
        try await restarted.logout()
        XCTAssertNil(try storage.read())
    }

    func testReplacementStopsBeforeBrowserWhenOldRevocationFails() async throws {
        let storage = MemoryStorage("old-session")
        let browser = FixtureBrowser { _ in XCTFail("Never authorize before old revocation succeeds"); return URL(string: "https://invalid.example")! }
        let transport = FixtureTransport(revocationResponse: { _ in AuthHTTPResponse(status: 503, body: Data()) }) { _ in
            XCTFail("No exchange before revocation"); throw AuthError.invalidToken
        }
        let session = AuthSession(transport: transport, browser: browser, storage: storage)
        do { try await session.login(); XCTFail("Replacement must abort") }
        catch AuthError.httpStatus(503) { }
        XCTAssertEqual(try storage.read(), RefreshCredential(token: "old-session", status: .pendingRevocation))
    }

    func testRotationPersistsBeforeUserInfoFailureWithoutTreatingHTTP400AsMissingLogin() async throws {
        let storage = MemoryStorage("old")
        let transport = FixtureTransport { request in
            if request.url!.path.hasSuffix("/userinfo") {
                XCTAssertEqual(try storage.read()?.token, "rotated")
                return AuthHTTPResponse(status: 400, body: Data())
            }
            return Self.tokens(access: "access", refresh: "rotated", idToken: nil)
        }
        let browser = FixtureBrowser { _ in XCTFail("Should not log in"); return URL(string: "https://invalid.example")! }
        let session = AuthSession(transport: transport, browser: browser, storage: storage)
        do { _ = try await session.verifiedSubject(); XCTFail("UserInfo fails") }
        catch AuthError.httpStatus(400) { }
        XCTAssertEqual(try storage.read(), RefreshCredential(token: "rotated"))
    }

    func testInvalidClientPreservesCredentialButInvalidGrantMakesItUnusable() async throws {
        let browser = FixtureBrowser { _ in XCTFail("Should not open"); return URL(string: "https://invalid.example")! }
        for code in ["invalid_client", "invalid_grant"] {
            let storage = MemoryStorage("saved")
            let transport = FixtureTransport { _ in
                AuthHTTPResponse(status: 400, body: Data("{\"error\":\"\(code)\"}".utf8))
            }
            let session = AuthSession(transport: transport, browser: browser, storage: storage)
            do { _ = try await session.bearerToken(); XCTFail("Expected OAuth rejection") }
            catch {
                XCTAssertEqual(error as? AuthError, code == "invalid_grant" ? .noSession : .oauth(status: 400, code: code))
            }
            XCTAssertEqual(try storage.read(), code == "invalid_grant" ? nil : RefreshCredential(token: "saved"))
        }
    }

    func testLostRefreshResponseNeverBecomesSuccessfulLogoutOrAutomaticReplay() async throws {
        let storage = MemoryStorage("possibly-consumed")
        let browser = FixtureBrowser { _ in XCTFail("Must not open"); return URL(string: "https://invalid.example")! }
        let transport = FixtureTransport { _ in throw URLError(.networkConnectionLost) }
        let session = AuthSession(transport: transport, browser: browser, storage: storage)
        do { _ = try await session.bearerToken(); XCTFail("Lost response") }
        catch let error as URLError { XCTAssertEqual(error.code, .networkConnectionLost) }
        XCTAssertEqual(try storage.read()?.status, .rotationUncertain)
        let restarted = AuthSession(transport: transport, browser: browser, storage: storage)
        do { _ = try await restarted.bearerToken(); XCTFail("Cannot replay uncertain rotation") }
        catch AuthError.rotationUncertain { }
        do { try await restarted.logout(); XCTFail("Revoking stale token cannot prove logout") }
        catch AuthError.rotationUncertain { }
        let posts = await transport.tokenRequests()
        let revocations = await transport.revocations()
        XCTAssertEqual(posts.count, 1)
        XCTAssertTrue(revocations.isEmpty)
    }

    func testDeniedPendingMarkerNeverReportsLogoutAndSuspendsInMemory() async throws {
        let storage = MemoryStorage("saved", refusesWrites: true)
        let transport = FixtureTransport { _ in XCTFail("No request after persistence failure"); throw AuthError.invalidToken }
        let browser = FixtureBrowser { _ in XCTFail("Must not authorize"); return URL(string: "https://invalid.example")! }
        let session = AuthSession(transport: transport, browser: browser, storage: storage)
        do { try await session.logout(); XCTFail("Keychain denial cannot be success") }
        catch AuthError.keychain(-25293) { }
        do { _ = try await session.bearerToken(); XCTFail("Must remain suspended in this process") }
        catch AuthError.pendingRevocation { }
        XCTAssertEqual(try storage.read(), RefreshCredential(token: "saved"))
        let revocations = await transport.revocations()
        XCTAssertTrue(revocations.isEmpty)
        // A failed durable write cannot guarantee suspension across a process restart.
    }

    func testOffIssuerRevocationEndpointIsRejectedBeforeAnyCredentialLeavesDevice() async throws {
        let storage = MemoryStorage("saved")
        let browser = FixtureBrowser { _ in XCTFail("Must not authorize"); return URL(string: "https://invalid.example")! }
        let session = AuthSession(transport: OffIssuerRevocationTransport(), browser: browser, storage: storage)
        do { _ = try await session.bearerToken(); XCTFail("Must reject off-issuer revocation") }
        catch AuthError.invalidDiscovery { }
        XCTAssertEqual(try storage.read(), RefreshCredential(token: "saved"))
    }

    private static func form(_ request: URLRequest) -> [String: String] {
        let string = String(data: request.httpBody!, encoding: .utf8)!
        let items = URLComponents(string: "?" + string)!.queryItems!
        return Dictionary(uniqueKeysWithValues: items.map { ($0.name, $0.value!) })
    }
    fileprivate static func tokens(access: String, refresh: String, idToken: String?) -> AuthHTTPResponse {
        var body: [String: Any] = ["access_token": access, "refresh_token": refresh,
                                   "token_type": "Bearer", "expires_in": 3600]
        if let idToken { body["id_token"] = idToken }
        return AuthHTTPResponse(status: 200, body: try! JSONSerialization.data(withJSONObject: body))
    }
    private static func idToken(nonce: String) -> String {
        let payload = try! JSONSerialization.data(withJSONObject: ["iss": OIDCConfiguration.alondra.issuer.absoluteString,
            "aud": "alondra-ios", "nonce": nonce, "exp": 2_000_003_600])
        return "eyJhbGciOiJSUzI1NiJ9." + payload.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "") + ".signature"
    }
}

private struct OffIssuerRevocationTransport: AuthHTTPTransport {
    func send(_ request: URLRequest) async throws -> AuthHTTPResponse {
        XCTAssertTrue(request.url!.path.hasSuffix("openid-configuration"))
        return AuthHTTPResponse(status: 200, body: Data("""
        {"issuer":"https://alondra-auth.davidcollado.dev/realms/alondra","authorization_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/auth","token_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/token","revocation_endpoint":"https://attacker.example/revoke"}
        """.utf8))
    }
}
