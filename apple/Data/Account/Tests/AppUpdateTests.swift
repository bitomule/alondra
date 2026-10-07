import AlondraAccount
import AlondraContract
import AlondraFeatures
import AlondraPairing
import Foundation
import Synchronization
import Testing

/// Exactly what 0.1.2 (build 4) wrote to the Keychain: the refresh credential item and the
/// approved pairing collection item. An update keeps both items; the new build must read them.
private enum Build4 {
    static let credential = #"{"token":"refresh-from-build-4","status":"active","version":1}"#
    static let pairings = #"{"pairings":[{"ownerSub":"owner-a","macSigningKey":"BObV_1DlZno1_L-gZxZf6UPE3xG4nezPAyR-7wwERu0MRPX0qrbaj1TdSCFwyL-pGOmJZTLNFGAHjldWVc7K-wY","signingPrivateKey":"0j\/VoQGXlPDuFyoHLYErjD8WBGmRuyWY2U2JxQTqJi4=","name":"Mac Studio","approvedAtMs":1791000000000,"macAgreementKey":"BA6JIIzc1VGvtPmNpUSgRETIFNoo2SEYhY0tc1aZIXtOsVR85NlNOGomVMpArUEVJn0k41dmKHGi7bQTycUlAWo","agreementPrivateKey":"JtPiLX6tGfLKm7nkwOKSg4MHiFtykAsnkvi218pFHjk=","relayURL":"https:\/\/alondra-api.davidcollado.dev","deviceID":"phone-1","macID":"mac-studio"}],"version":1,"selectedID":{"deviceID":"phone-1","macID":"mac-studio","relayURL":"https:\/\/alondra-api.davidcollado.dev","ownerSub":"owner-a"}}"#
}

/// Holds the credential as the Keychain does: bytes, read back through the persisted format.
private final class KeychainBytes: RefreshTokenStorage, @unchecked Sendable {
    private let bytes: Mutex<Data?>
    init(_ value: String) { bytes = Mutex(Data(value.utf8)) }
    func read() throws -> RefreshCredential? { try bytes.withLock { try $0.map(RefreshCredential.decodePersisted) } }
    func write(_ credential: RefreshCredential) throws { let data = try credential.persistedData(); bytes.withLock { $0 = data } }
    func delete() throws { bytes.withLock { $0 = nil } }
}

private final class SavedPairings: ApprovedPairingStore {
    private let value: Mutex<ApprovedPairingCollection>
    private let deleted = Mutex(false)
    init(_ json: String) throws { value = Mutex(try JSONDecoder().decode(ApprovedPairingCollection.self, from: Data(json.utf8))) }
    var wasDeleted: Bool { deleted.withLock { $0 } }
    func loadCollection() throws -> ApprovedPairingCollection { value.withLock { $0 } }
    func load() throws -> ApprovedServicePairing? { value.withLock { $0.selected } }
    func save(_ pairing: ApprovedServicePairing) throws { try value.withLock { try $0.upsert(pairing) } }
    func select(_ id: ApprovedPairingID) throws { try value.withLock { try $0.select(id) } }
    func restoreSelection(_ previous: ApprovedPairingID, ifSelected attempted: ApprovedPairingID) throws {}
    func delete() throws { deleted.withLock { $0 = true }; value.withLock { $0 = ApprovedPairingCollection() } }
    func modify(_ change: (inout ApprovedPairingCollection) throws -> Void) throws {
        try value.withLock { current in
            var next = current
            try change(&next)
            current = next
        }
    }
}

/// The sign-in server after the update: renews the saved token and answers userinfo with
/// `userInfoStatus` (200 as owner-a, or a refusal as Keycloak gives right after a restart).
private struct Issuer: AuthHTTPTransport {
    let userInfoStatus: Int
    func send(_ request: URLRequest) async throws -> AuthHTTPResponse {
        let path = request.url!.path
        if path.hasSuffix("openid-configuration") {
            return AuthHTTPResponse(status: 200, body: Data("""
            {"issuer":"https://alondra-auth.davidcollado.dev/realms/alondra","authorization_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/auth","token_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/token","revocation_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/revoke","userinfo_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/userinfo"}
            """.utf8))
        }
        if path.hasSuffix("/token") {
            return AuthHTTPResponse(status: 200, body: try JSONSerialization.data(withJSONObject: [
                "access_token": "access", "refresh_token": "refresh-from-build-4", "token_type": "Bearer", "expires_in": 300]))
        }
        if path.hasSuffix("/userinfo") {
            return AuthHTTPResponse(status: userInfoStatus, body: Data(#"{"sub":"owner-a"}"#.utf8))
        }
        Issue.record("Unexpected request to \(path)")
        return AuthHTTPResponse(status: 500, body: Data())
    }
}

private struct NoBrowser: AuthBrowser {
    func authenticate(url: URL, callbackScheme: String, generation: Int) async throws -> URL {
        Issue.record("An update must not ask to sign in")
        throw CancellationError()
    }
    func cancelAuthentication(generation: Int) async {}
}

@MainActor
private func waitUntil(_ condition: @MainActor () -> Bool) async throws {
    for _ in 0..<300 where !condition() { try await Task.sleep(for: .milliseconds(10)) }
}

@MainActor
struct AppUpdateTests {
    /// First launch of the new build: new process, new AuthSession and AccountSession, the
    /// Keychain items the previous build left.
    private func firstLaunch(userInfoStatus: Int) throws -> (AccountSession, KeychainBytes, SavedPairings) {
        let credential = KeychainBytes(Build4.credential)
        let pairings = try SavedPairings(Build4.pairings)
        let auth = AuthSession(transport: Issuer(userInfoStatus: userInfoStatus), browser: NoBrowser(),
                               storage: credential, diagnostics: { _, _ in })
        let account = AccountSession(auth: auth, displayName: "iPhone",
                                     storageDirectory: FileManager.default.temporaryDirectory.appending(path: "update-\(UUID().uuidString)"),
                                     pairingStore: pairings, browse: { AsyncStream { $0.finish() } })
        return (account, credential, pairings)
    }

    @Test func anUpdateKeepsTheSessionAndThePairing() async throws {
        let (account, credential, pairings) = try firstLaunch(userInfoStatus: 200)
        account.store.send(.appeared)
        try await waitUntil { account.store.phase == .approved }

        #expect(account.store.phase == .approved)
        #expect(account.store.restoreProblem == nil)
        #expect(!account.store.reauthenticationRequired)
        #expect(account.macs.map(\.macID) == ["mac-studio"])
        #expect(try credential.read() == RefreshCredential(token: "refresh-from-build-4"))
        #expect(!pairings.wasDeleted)
    }

    /// The Pair Again the apps put on a Mac's connection screen: a device removed through the
    /// relay never gets a sealed revocation, so it must have it too.
    @Test func aRelayMacOffersPairAgainAndItTargetsThatMac() async throws {
        let (account, _, pairings) = try firstLaunch(userInfoStatus: 200)
        let approved = try #require(try pairings.load())
        let services = try Connection(approved: approved, auth: account.auth)
        #expect(!services.isDirectOnly)
        let opened = Mutex(false)
        let action = try #require(account.pairAgainAction(for: services) { opened.withLock { $0 = true } })
        action()
        #expect(account.pairAgainMacID == "mac-studio")
        #expect(opened.withLock { $0 })
    }

    @Test func anUpdateWhileTheSignInServerRestartsStillKeepsTheSession() async throws {
        let (account, credential, pairings) = try firstLaunch(userInfoStatus: 401)
        account.store.send(.appeared)
        try await waitUntil { account.store.restoreProblem != nil }

        #expect(account.store.restoreProblem == .accessNotConfirmed)
        #expect(!account.store.reauthenticationRequired)
        #expect(!account.store.logoutIncomplete)
        #expect(try credential.read() == RefreshCredential(token: "refresh-from-build-4"))
        #expect(!pairings.wasDeleted)
    }
}
