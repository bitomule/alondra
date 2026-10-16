import AlondraTestSupport
import AlondraTransport
import CryptoKit
import Foundation
import SQLite3
import Testing

private actor LostResponseTransport: RelayRequestTransport {
    private(set) var postCount = 0
    private let response: Envelope?

    init(response: Envelope? = nil) {
        self.response = response
    }

    func submitRequest(_ envelope: Envelope) async throws -> RelayReceipt {
        postCount += 1
        throw URLError(.timedOut)
    }

    func requestStatus(requestID: String) async throws -> RelayReceipt {
        RelayReceipt(requestID: requestID, status: "completed", revision: 7, response: response)
    }
}

private actor DelayedStatusTransport: RelayRequestTransport {
    private var calls = 0
    private var started = false
    private var startedWaiters: [CheckedContinuation<Void, Never>] = []
    private var olderWaiter: CheckedContinuation<RelayReceipt, Never>?

    func submitRequest(_ envelope: Envelope) async throws -> RelayReceipt {
        throw URLError(.timedOut)
    }

    func requestStatus(requestID: String) async throws -> RelayReceipt {
        calls += 1
        if calls == 1 {
            started = true
            for waiter in startedWaiters { waiter.resume() }
            startedWaiters.removeAll()
            return await withCheckedContinuation { olderWaiter = $0 }
        }
        return RelayReceipt(requestID: requestID, status: "completed", revision: 11, response: nil)
    }

    func waitForOlderStatus() async {
        if started { return }
        await withCheckedContinuation { startedWaiters.append($0) }
    }

    func releaseOlderStatus(requestID: String) {
        olderWaiter?.resume(returning: RelayReceipt(
            requestID: requestID, status: "received", revision: 10, response: nil))
        olderWaiter = nil
    }
}

private actor DelayedPostTransport: RelayRequestTransport {
    private var started = false
    private var startedWaiters: [CheckedContinuation<Void, Never>] = []
    private var postWaiter: CheckedContinuation<Void, Never>?

    func submitRequest(_ envelope: Envelope) async throws -> RelayReceipt {
        started = true
        for waiter in startedWaiters { waiter.resume() }
        startedWaiters.removeAll()
        await withCheckedContinuation { postWaiter = $0 }
        throw URLError(.timedOut)
    }

    func requestStatus(requestID: String) async throws -> RelayReceipt {
        RelayReceipt(requestID: requestID, status: "completed", revision: 11, response: nil)
    }

    func waitForPost() async {
        if started { return }
        await withCheckedContinuation { startedWaiters.append($0) }
    }

    func releasePost() {
        postWaiter?.resume()
        postWaiter = nil
    }
}

@Suite(.temporaryFiles) struct RelayOutboxIntegrationTests {
    private let scope = RelayOutboxScope(
        relay: URL(string: "https://relay.example.test")!, macID: "mac-1", deviceID: "phone-1",
        credentialRevision: "account-session-1"
    )

    private func envelope(_ id: String) throws -> Envelope {
        let phone = P256.Signing.PrivateKey()
        let mac = P256.KeyAgreement.PrivateKey()
        return try Envelope.seal(
            route: "mac-1", sender: "phone-1", recipient: "mac-1", requestID: id,
            kind: "request", expiresAtMilliseconds: UInt64(Date().timeIntervalSince1970 * 1000) + 60_000,
            plaintext: Data("private report".utf8), senderKey: phone, recipientKey: mac.publicKey
        )
    }

    private func attempt(_ outbox: RelayOutbox, envelope: Envelope) async -> Result<RelaySubmission, Error> {
        do { return .success(try await outbox.submitNew(envelope)) }
        catch { return .failure(error) }
    }

    @Test func lostResponseIsReconciledByStatusWithoutReposting() async throws {
        let id = UUID().uuidString
        let sealed = try envelope(id)
        let transport = LostResponseTransport(response: sealed)
        let outbox = RelayOutbox(scope: scope, transport: transport)

        guard case .uncertain = try await outbox.submitNew(sealed) else {
            Issue.record("A lost POST response must be uncertain")
            return
        }
        #expect(try await outbox.entry(requestID: id)?.phase == .uncertain)
        await #expect(throws: RelayOutboxError.duplicateRequestID) {
            try await outbox.submitNew(sealed)
        }
        #expect(await transport.postCount == 1)

        let receipt = try await outbox.reconcile(requestID: id)
        #expect(receipt?.status == "completed")
        #expect(try await outbox.entry(requestID: id)?.phase == .acknowledged)
        #expect(try await outbox.entry(requestID: id)?.lastResponse == sealed)
        #expect(await transport.postCount == 1)

        try await outbox.retireAppliedResponse(requestID: id, revision: 7, verifiedResponse: sealed)
        let tombstone = try await outbox.entry(requestID: id)
        #expect(tombstone?.phase == .retired)
        #expect(tombstone?.envelope == nil)
        #expect(tombstone?.lastResponse == nil)
        if try await outbox.reconcile(requestID: id) != nil {
            Issue.record("A retired response must not be presented for application again")
        }
        await #expect(throws: RelayOutboxError.duplicateRequestID) {
            try await outbox.submitNew(sealed)
        }
        #expect(await transport.postCount == 1)
    }

    @Test func anEnvelopeForAnotherDeviceIsRefusedBeforePOST() async throws {
        let transport = LostResponseTransport()
        let other = RelayOutbox(scope: RelayOutboxScope(relay: scope.relay, macID: scope.macID, deviceID: "phone-2",
                                                        credentialRevision: scope.credentialRevision), transport: transport)
        await #expect(throws: RelayOutboxError.wrongScope) {
            try await other.submitNew(envelope(UUID().uuidString))
        }
        #expect(await transport.postCount == 0)
    }

    @Test func concurrentSubmissionsOfOneRequestPostItOnce() async throws {
        let id = UUID().uuidString
        let sealed = try envelope(id)
        let transport = LostResponseTransport()
        let outbox = RelayOutbox(scope: scope, transport: transport)

        async let firstResult = attempt(outbox, envelope: sealed)
        async let secondResult = attempt(outbox, envelope: sealed)
        let outcomes = await [firstResult, secondResult]
        #expect(outcomes.filter { if case .success(.uncertain) = $0 { true } else { false } }.count == 1)
        #expect(outcomes.filter {
            if case .failure(let error) = $0 { return error as? RelayOutboxError == .duplicateRequestID }
            return false
        }.count == 1)
        #expect(await transport.postCount == 1)
    }

    @Test func localRelayRejectionsNeverReserveAnID() async throws {
        let id = UUID().uuidString
        let valid = try envelope(id)
        let transport = LostResponseTransport()
        let outbox = RelayOutbox(scope: scope, transport: transport)

        let wrongKind = Envelope(
            route: valid.route, sender: valid.sender, recipient: valid.recipient,
            requestID: id, kind: "response", expiresAtMilliseconds: valid.expiresAtMilliseconds,
            ephemeralKey: valid.ephemeralKey, nonce: valid.nonce,
            ciphertext: valid.ciphertext, signature: valid.signature)
        await #expect(throws: RelayOutboxError.invalidEnvelope) {
            try await outbox.submitNew(wrongKind)
        }
        let oversized = Envelope(
            route: valid.route, sender: valid.sender, recipient: valid.recipient,
            requestID: id, kind: "request", expiresAtMilliseconds: valid.expiresAtMilliseconds,
            ephemeralKey: valid.ephemeralKey, nonce: valid.nonce,
            ciphertext: String(repeating: "A", count: 400_000), signature: valid.signature)
        await #expect(throws: RelayOutboxError.invalidEnvelope) {
            try await outbox.submitNew(oversized)
        }
        #expect(try await outbox.entry(requestID: id) == nil)
        #expect(await transport.postCount == 0)
    }

    @Test func olderStatusCannotRollBackOrReturnStaleRevision() async throws {
        let id = UUID().uuidString
        let transport = DelayedStatusTransport()
        let outbox = RelayOutbox(scope: scope, transport: transport)
        guard case .uncertain = try await outbox.submitNew(envelope(id)) else {
            Issue.record("The lost POST response must remain uncertain")
            return
        }

        let older = Task { try await outbox.reconcile(requestID: id) }
        await transport.waitForOlderStatus()
        let newer = try await outbox.reconcile(requestID: id)
        #expect(newer?.revision == 11)
        await transport.releaseOlderStatus(requestID: id)
        let delayed = try await older.value
        #expect(delayed?.revision == 11)
        #expect(try await outbox.entry(requestID: id)?.lastRevision == 11)
    }

    @Test func latePostTimeoutCannotDowngradeReconciledResult() async throws {
        let id = UUID().uuidString
        let transport = DelayedPostTransport()
        let outbox = RelayOutbox(scope: scope, transport: transport)
        let sealed = try envelope(id)
        let posting = Task { try await outbox.submitNew(sealed) }
        await transport.waitForPost()
        #expect(try await outbox.reconcile(requestID: id)?.revision == 11)
        await transport.releasePost()
        guard case .uncertain = try await posting.value else {
            Issue.record("A late transport timeout remains conservative for its caller")
            return
        }
        #expect(try await outbox.entry(requestID: id)?.phase == .acknowledged)
        #expect(try await outbox.entry(requestID: id)?.lastRevision == 11)
    }
    @Test func relaunchKeepsUncertainRequestAndRetiredTombstone() async throws {
        let directory = try TestFiles.directory()
        let id = UUID().uuidString
        let sealed = try envelope(id)
        let tag = Data("opaque keyed binding".utf8)
        let transport = LostResponseTransport(response: sealed)
        let first = RelayOutbox(scope: scope, transport: transport,
            journal: try EnvelopeJournal(scope: scope, directory: directory))
        _ = try await first.submitNew(sealed, semanticTag: tag)
        let relaunched = RelayOutbox(scope: scope, transport: transport,
            journal: try EnvelopeJournal(scope: scope, directory: directory))
        #expect(try await relaunched.entry(requestID: id)?.envelope == sealed)
        #expect(try await relaunched.entry(requestID: id)?.semanticTag == tag)
        #expect(try await relaunched.reconcile(requestID: id)?.revision == 7)
        try await relaunched.retireAppliedResponse(requestID: id, revision: 7, verifiedResponse: sealed)
        let third = RelayOutbox(scope: scope, transport: transport,
            journal: try EnvelopeJournal(scope: scope, directory: directory))
        #expect(try await third.entry(requestID: id)?.phase == .retired)
        #expect(try await third.entry(requestID: id)?.envelope == nil)
        await #expect(throws: RelayOutboxError.duplicateRequestID) { try await third.submitNew(sealed) }
        #expect(await transport.postCount == 1)
    }

    @Test func separateJournalsCannotDispatchTheSameRequestTwice() async throws {
        let directory = try TestFiles.directory()
        let id = UUID().uuidString
        let sealed = try envelope(id)
        let transport = LostResponseTransport()
        let first = RelayOutbox(scope: scope, transport: transport,
            journal: try EnvelopeJournal(scope: scope, directory: directory))
        let second = RelayOutbox(scope: scope, transport: transport,
            journal: try EnvelopeJournal(scope: scope, directory: directory))
        async let a = attempt(first, envelope: sealed)
        async let b = attempt(second, envelope: sealed)
        let results = await [a, b]
        #expect(results.filter { if case .success(.uncertain) = $0 { true } else { false } }.count == 1)
        #expect(results.filter {
            if case .failure(let error) = $0 { return error as? RelayOutboxError == .duplicateRequestID }
            return false
        }.count == 1)
        #expect(await transport.postCount == 1)
        #expect(try await first.entry(requestID: id)?.envelope == sealed)
    }

    /// Under memory pressure a writer from another journal can hold the database lock for seconds;
    /// the journal waited one second and then refused the operation as unavailable.
    @Test func aJournalWaitsForAnotherJournalsLongTransaction() async throws {
        let directory = try TestFiles.directory()
        let journal = try EnvelopeJournal(scope: scope, directory: directory)
        _ = try await journal.entries()
        let namespace = try #require(FileManager.default.contentsOfDirectory(at: directory,
            includingPropertiesForKeys: nil).first)
        var holder: OpaquePointer?
        try #require(sqlite3_open(namespace.appendingPathComponent("operations.sqlite").path, &holder) == SQLITE_OK)
        defer { sqlite3_close(holder) }
        try #require(sqlite3_exec(holder, "BEGIN IMMEDIATE", nil, nil, nil) == SQLITE_OK)

        let waiting = Task { try await journal.entry(requestID: "held") }
        try await Task.sleep(for: .seconds(1.5))
        try #require(sqlite3_exec(holder, "COMMIT", nil, nil, nil) == SQLITE_OK)

        #expect(try await waiting.value == nil)
    }

    /// Every iOS container is reached through /var, a link to /private/var: SQLite's NOFOLLOW refused
    /// the whole path, so the iPhone never prepared an answer and none reached the Mac.
    @Test func aJournalReachedThroughALinkedFolderKeepsAndRetiresOperations() async throws {
        let root = try TestFiles.directory()
        let real = root.appending(path: "private", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: real, withIntermediateDirectories: true)
        let linked = root.appending(path: "var")
        try FileManager.default.createSymbolicLink(at: linked, withDestinationURL: real)
        let transport = LostResponseTransport()
        let journal = try EnvelopeJournal(scope: scope, directory: linked.appending(path: "operations"))

        _ = try await RelayOutbox(scope: scope, transport: transport, journal: journal).submitNew(envelope("answer"))
        try await journal.retire(ActionJournalAcknowledgment(dataEpoch: "epoch", generation: 1, ranges: []))

        let reopened = try EnvelopeJournal(scope: scope, directory: real.appending(path: "operations"))
        #expect(try await reopened.entry(requestID: "answer")?.envelope != nil)
        #expect(await transport.postCount == 1)
    }

    @Test func lostDatabaseIsRefusedBeforeAnyNewDispatch() async throws {
        let directory = try TestFiles.directory()
        let journal = try EnvelopeJournal(scope: scope, directory: directory)
        let transport = LostResponseTransport()
        let first = RelayOutbox(scope: scope, transport: transport, journal: journal)
        _ = try await first.submitNew(envelope("original"))
        let namespace = try #require(FileManager.default.contentsOfDirectory(at: directory,
            includingPropertiesForKeys: nil).first)
        try FileManager.default.removeItem(at: namespace.appendingPathComponent("operations.sqlite"))
        let relaunched = RelayOutbox(scope: scope, transport: transport,
            journal: try EnvelopeJournal(scope: scope, directory: directory))
        await #expect(throws: EnvelopeJournalError.corrupt) { try await relaunched.submitNew(envelope("next")) }
        #expect(await transport.postCount == 1)
    }

    @Test func credentialsIsolateJournalsAndDiskContainsOnlyCiphertext() async throws {
        let directory = try TestFiles.directory()
        let transport = LostResponseTransport()
        let first = RelayOutbox(scope: scope, transport: transport,
            journal: try EnvelopeJournal(scope: scope, directory: directory))
        _ = try await first.submitNew(envelope("same-id"))
        let namespace = try #require(FileManager.default.contentsOfDirectory(at: directory,
            includingPropertiesForKeys: nil).first)
        let changed = RelayOutboxScope(relay: scope.relay, macID: scope.macID,
            deviceID: scope.deviceID, credentialRevision: "replacement-key")
        let other = try EnvelopeJournal(scope: changed, directory: directory)
        #expect(try await other.entry(requestID: "same-id") == nil)
        let bytes = try Data(contentsOf: namespace.appendingPathComponent("operations.sqlite"))
        #expect(bytes.range(of: Data("private report".utf8)) == nil)
    }

}
