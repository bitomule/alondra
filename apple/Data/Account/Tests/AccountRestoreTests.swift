import AlondraTestSupport
import AlondraAccount
@testable import AlondraNotifications
import AlondraPairing
import AlondraTransport
import CryptoKit
import AlondraFeatures
import Foundation
import Network
import Synchronization
import Testing

private final class MemoryPairings: ApprovedPairingStore {
    private let value: Mutex<ApprovedPairingCollection>
    init(_ pairings: [ApprovedServicePairing]) throws {
        var collection = ApprovedPairingCollection()
        for pairing in pairings { try collection.upsert(pairing) }
        value = Mutex(collection)
    }
    func loadCollection() throws -> ApprovedPairingCollection { value.withLock { $0 } }
    func load() throws -> ApprovedServicePairing? { value.withLock { $0.selected } }
    func save(_ pairing: ApprovedServicePairing) throws { try value.withLock { try $0.upsert(pairing) } }
    func select(_ id: ApprovedPairingID) throws { try value.withLock { try $0.select(id) } }
    func restoreSelection(_ previous: ApprovedPairingID, ifSelected attempted: ApprovedPairingID) throws {}
    func delete() throws { value.withLock { $0 = ApprovedPairingCollection() } }
    func modify(_ change: (inout ApprovedPairingCollection) throws -> Void) throws {
        try value.withLock { current in
            var next = current
            try change(&next)
            current = next
        }
    }
}

private final class MemoryCredential: RefreshTokenStorage, @unchecked Sendable {
    private let lock = NSLock()
    private var credential: RefreshCredential?
    init(_ credential: RefreshCredential?) { self.credential = credential }
    func read() throws -> RefreshCredential? { lock.withLock { credential } }
    func write(_ credential: RefreshCredential) throws { lock.withLock { self.credential = credential } }
    func delete() throws { lock.withLock { credential = nil } }
}

private final class Shared<Value: Sendable>: Sendable {
    private let value: Mutex<Value>
    init(_ value: Value) { self.value = Mutex(value) }
    var current: Value { value.withLock { $0 } }
    func set(_ newValue: Value) { value.withLock { $0 = newValue } }
}

private struct Offline: AuthHTTPTransport {
    func send(_ request: URLRequest) async throws -> AuthHTTPResponse { throw URLError(.notConnectedToInternet) }
}

private struct NoBrowser: AuthBrowser {
    func authenticate(url: URL, callbackScheme: String, generation: Int) async throws -> URL { throw CancellationError() }
    func cancelAuthentication(generation: Int) async {}
}

private func wire(_ key: P256.Signing.PublicKey) -> String {
    key.x963Representation.base64EncodedString().replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
}

private func wire(_ key: P256.KeyAgreement.PublicKey) -> String {
    key.x963Representation.base64EncodedString().replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
}

private func pairing(_ mac: String, relay: Bool) -> ApprovedServicePairing {
    var approved = ApprovedServicePairing(
        relayURL: relay ? URL(string: "https://alondra-api.davidcollado.dev") : nil, macID: mac,
        deviceID: "phone-" + mac, ownerSub: relay ? "owner-a" : nil,
        macSigningKey: wire(P256.Signing.PrivateKey().publicKey), macAgreementKey: wire(P256.KeyAgreement.PrivateKey().publicKey),
        signingPrivateKey: P256.Signing.PrivateKey().rawRepresentation,
        agreementPrivateKey: P256.KeyAgreement.PrivateKey().rawRepresentation, approvedAtMs: 1)
    approved.name = mac
    return approved
}

@MainActor
private func account(credential: RefreshCredential?, pairings: [ApprovedServicePairing],
                     advertised: [DiscoveredMac] = [], localNetwork: LocalNetworkAccess = MemoryLocalNetworkAccess().access,
                     browse: (@Sendable () -> AsyncStream<MacBrowser.Update>)? = nil) throws -> AccountSession {
    let auth = AuthSession(transport: Offline(), browser: NoBrowser(), storage: MemoryCredential(credential))
    let owner = AccountSession(auth: auth, displayName: "iPhone",
                               storageDirectory: try TestFiles.directory(),
                               pairingStore: try MemoryPairings(pairings), localNetwork: localNetwork, browse: browse ?? {
                                   AsyncStream { continuation in
                                       continuation.yield(.found(advertised))
                                       continuation.finish()
                                   }
                               })
    try TestFiles.beforeRemoval { try await owner.stop() }
    return owner
}

private func advertised(_ name: String, macID: String) -> DiscoveredMac {
    DiscoveredMac(id: name, name: name, macID: macID,
                  endpoint: .service(name: name, type: "_alondra._tcp", domain: "local.", interface: nil))
}

@MainActor
private func waitUntil(_ condition: @MainActor () -> Bool) async throws {
    for _ in 0..<300 where !condition() { try await Task.sleep(for: .milliseconds(10)) }
}

@MainActor
private final class StartupPermissionGate {
    let entered: AsyncStream<Void>
    private let signal: AsyncStream<Void>.Continuation
    private var waiter: CheckedContinuation<Void, Never>?
    init() { (entered, signal) = AsyncStream.makeStream() }
    func request() async {
        await withCheckedContinuation { waiter = $0; signal.yield(()) }
    }
    func release() { waiter?.resume(); waiter = nil }
}

@MainActor
@Suite(.temporaryFiles)
struct AccountRestoreTests {
    @Test func theFirstConnectionIsPublishedBeforeTheNotificationPermissionDecision() async throws {
        let activation = DeferredPushActivation(), permission = StartupPermissionGate()
        let auth = AuthSession(transport: Offline(), browser: NoBrowser(), storage: MemoryCredential(nil))
        var registered = false
        let owner = AccountSession(auth: auth, displayName: "Fixture client",
            storageDirectory: try TestFiles.directory(), didReconcile: { _ in
                let commit = await activation.prepare { isCurrent in
                    await permission.request()
                    if isCurrent() { registered = true }
                }
                return { commit() }
            }, pairingStore: try MemoryPairings([pairing("local-fixture", relay: false)]),
            localNetwork: MemoryLocalNetworkAccess().access, browse: { AsyncStream { $0.finish() } })
        try TestFiles.beforeRemoval { try await owner.stop() }
        var entered = permission.entered.makeAsyncIterator()
        let restoration = Task { try await owner.prepareNotificationSession() }
        _ = await entered.next()
        let publishedBeforeDecision = owner.services != nil
        let macsBeforeDecision = owner.macs.map(\.macID)
        #expect(!registered)
        permission.release()
        try await restoration.value
        await activation.drain()
        #expect(publishedBeforeDecision)
        #expect(macsBeforeDecision == ["local-fixture"])
        #expect(registered)
    }

    @Test func startingWithoutNetworkKeepsBothKindsOfMac() async throws {
        let owner = try account(credential: RefreshCredential(token: "refresh"),
                                pairings: [pairing("mac-home", relay: false), pairing("mac-office", relay: true)])
        owner.store.send(.appeared)
        try await waitUntil { owner.macs.count == 2 }
        #expect(Set(owner.macs.map(\.macID)) == ["mac-home", "mac-office"])
        #expect(owner.services != nil)
    }

    @Test func anAccountThatNeedsSigningInAgainStillKeepsItsMacsBesideTheLocalOnes() async throws {
        let owner = try account(credential: nil,
                                pairings: [pairing("mac-home", relay: false), pairing("mac-office", relay: true)])
        owner.store.send(.appeared)
        try await waitUntil { owner.macs.count == 2 }
        #expect(Set(owner.macs.map(\.macID)) == ["mac-home", "mac-office"])
    }

    @Test func aSignOutThatNeverFinishedIsNotSwallowedByTheLocalMacs() async throws {
        let owner = try account(credential: RefreshCredential(token: "refresh", status: .pendingRevocation),
                                pairings: [pairing("mac-home", relay: false), pairing("mac-office", relay: true)])
        owner.store.send(.appeared)
        try await waitUntil { owner.store.logoutIncomplete }
        #expect(owner.store.logoutIncomplete)
        #expect(owner.services == nil)
    }

    @Test func withOnlyLocalMacsTheAccountIsNeverAsked() async throws {
        let owner = try account(credential: RefreshCredential(token: "refresh", status: .pendingRevocation),
                                pairings: [pairing("mac-home", relay: false)])
        owner.store.send(.appeared)
        try await waitUntil { owner.macs.count == 1 || owner.store.logoutIncomplete }
        #expect(owner.macs.map(\.macID) == ["mac-home"])
    }

    @Test func findAMacNeverOffersTheMacItRunsOn() async throws {
        let owner = try account(credential: nil, pairings: [],
                                advertised: [advertised("This one", macID: "mac-self"), advertised("Studio", macID: "mac-studio")])
        owner.ownMacID = "mac-self"
        var seen: [FoundMac] = []
        for await update in owner.findMacClient.browse() {
            if case let .macs(macs) = update { seen = macs }
        }
        #expect(seen.map(\.name) == ["Studio"])
    }

    @Test func aSavedMacIsOfferedAgainOnlyWhileThePersonPairsItAgain() async throws {
        let owner = try account(credential: nil, pairings: [pairing("mac-studio", relay: false), pairing("mac-mini", relay: false)],
                                advertised: [advertised("Studio", macID: "mac-studio"), advertised("Mini", macID: "mac-mini")])
        owner.store.send(.appeared)
        try await waitUntil { owner.macs.count == 2 }
        func pairedFlags() async -> [String: Bool] {
            var seen: [FoundMac] = []
            for await update in owner.findMacClient.browse() {
                if case let .macs(macs) = update { seen = macs }
            }
            return Dictionary(uniqueKeysWithValues: seen.map { ($0.name, $0.paired) })
        }
        #expect(await pairedFlags() == ["Studio": true, "Mini": true])
        owner.pairAgain(macID: "mac-studio")
        #expect(await pairedFlags() == ["Studio": false, "Mini": true])
        await owner.additionalMacPairing.cancel()
        #expect(await pairedFlags() == ["Studio": true, "Mini": true])
    }
}

@MainActor
@Suite(.temporaryFiles)
struct LocalNetworkAccessTests {
    @Test func aRefusalWhileTheSystemAlertIsUpIsNotTheAnswer() async throws {
        let browsed = Shared(0), saved = MemoryLocalNetworkAccess(.unknown).access
        let owner = try account(credential: nil, pairings: [], localNetwork: saved, browse: {
            browsed.set(browsed.current + 1)
            return AsyncStream { continuation in
                continuation.yield(.denied)
                continuation.finish()
            }
        })
        let active = Shared(false)
        owner.isActive = { active.current }
        let updates = Task { @MainActor in
            var seen: [FindMacUpdate] = []
            for await update in owner.findMacClient.browse() { seen.append(update) }
            return seen
        }
        try await waitUntil { browsed.current == 1 }
        try await Task.sleep(for: .milliseconds(100))
        #expect(saved.state == .unknown)
        #expect(browsed.current == 1)
        active.set(true)
        let seen = await updates.value
        #expect(seen == [.denied])
        #expect(browsed.current == 2)
        #expect(saved.state == .denied)
    }

    @Test func allowingAccessAfterARefusalIsRemembered() async throws {
        let saved = MemoryLocalNetworkAccess(.denied).access
        let owner = try account(credential: nil, pairings: [], localNetwork: saved, browse: {
            AsyncStream { continuation in
                continuation.yield(.allowed)
                continuation.finish()
            }
        })
        var seen: [FindMacUpdate] = []
        for await update in owner.findMacClient.browse() { seen.append(update) }
        #expect(seen == [.allowed])
        #expect(saved.state == .allowed)
    }
}
