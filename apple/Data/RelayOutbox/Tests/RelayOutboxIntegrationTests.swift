import AlondraRelayClient
import AlondraRelayOutbox
import CryptoKit
import Foundation
import Testing

private actor LostResponseTransport: RelayRequestTransport {
    private(set) var postCount = 0
    private let journalFile: URL
    private let expectedID: String
    private let response: EnvelopeV1?

    init(journalFile: URL, expectedID: String, response: EnvelopeV1? = nil) {
        self.journalFile = journalFile
        self.expectedID = expectedID
        self.response = response
    }

    func submitRequest(_ envelope: EnvelopeV1) async throws -> RelayReceipt {
        postCount += 1
        let bytes = try Data(contentsOf: journalFile)
        let text = try #require(String(data: bytes, encoding: .utf8))
        #expect(text.contains(expectedID), "The envelope must be on disk before POST")
        #expect(!text.contains("private report"), "The journal must contain ciphertext only")
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

    func submitRequest(_ envelope: EnvelopeV1) async throws -> RelayReceipt {
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

    func submitRequest(_ envelope: EnvelopeV1) async throws -> RelayReceipt {
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

@Suite struct RelayOutboxIntegrationTests {
    private let scope = RelayOutboxScope(
        relay: URL(string: "https://relay.example.test")!, macID: "mac-1", deviceID: "phone-1",
        credentialRevision: "account-session-1"
    )

    private func envelope(_ id: String) throws -> EnvelopeV1 {
        let phone = P256.Signing.PrivateKey()
        let mac = P256.KeyAgreement.PrivateKey()
        return try EnvelopeV1.seal(
            route: "mac-1", sender: "phone-1", recipient: "mac-1", requestID: id,
            kind: "request", expiresAtMilliseconds: UInt64(Date().timeIntervalSince1970 * 1000) + 60_000,
            plaintext: Data("private report".utf8), senderKey: phone, recipientKey: mac.publicKey
        )
    }

    private func attempt(_ outbox: RelayOutbox, envelope: EnvelopeV1) async -> Result<RelaySubmission, Error> {
        do { return .success(try await outbox.submitNew(envelope)) }
        catch { return .failure(error) }
    }

    @Test func lostResponseSurvivesRelaunchWithoutReposting() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("relay-outbox.json")
        let id = UUID().uuidString
        let sealed = try envelope(id)
        let transport = LostResponseTransport(journalFile: file, expectedID: id, response: sealed)
        let first = RelayOutbox(file: file, scope: scope, transport: transport)

        guard case .uncertain = try await first.submitNew(sealed) else {
            Issue.record("A lost POST response must be uncertain")
            return
        }
        let relaunched = RelayOutbox(file: file, scope: scope, transport: transport)
        #expect(try await relaunched.entry(requestID: id)?.phase == .uncertain)
        await #expect(throws: RelayOutboxError.duplicateRequestID) {
            try await relaunched.submitNew(sealed)
        }
        #expect(await transport.postCount == 1)

        let differentAccount = RelayOutbox(file: file, scope: RelayOutboxScope(
            relay: scope.relay, macID: scope.macID, deviceID: scope.deviceID,
            credentialRevision: "account-session-2"), transport: transport)
        await #expect(throws: RelayOutboxError.wrongScope) {
            try await differentAccount.reconcile(requestID: id)
        }
        #expect(await transport.postCount == 1)

        let receipt = try await relaunched.reconcile(requestID: id)
        #expect(receipt?.status == "completed")
        #expect(try await relaunched.entry(requestID: id)?.phase == .acknowledged)
        #expect(try await relaunched.entry(requestID: id)?.lastResponse == sealed)
        #expect(await transport.postCount == 1)

        try await relaunched.retireAppliedResponse(requestID: id, revision: 7,
                                                  verifiedResponse: sealed)
        let tombstone = try await relaunched.entry(requestID: id)
        #expect(tombstone?.phase == .retired)
        #expect(tombstone?.envelope == nil)
        #expect(tombstone?.lastResponse == nil)
        if try await relaunched.reconcile(requestID: id) != nil {
            Issue.record("A retired response must not be presented for application again")
        }
        await #expect(throws: RelayOutboxError.duplicateRequestID) {
            try await relaunched.submitNew(sealed)
        }
        #expect(await transport.postCount == 1)
    }

    @Test func corruptJournalFailsClosedBeforePOST() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let file = directory.appendingPathComponent("relay-outbox.json")
        try Data("not json".utf8).write(to: file)
        let id = UUID().uuidString
        let transport = LostResponseTransport(journalFile: file, expectedID: id)
        let outbox = RelayOutbox(file: file, scope: scope, transport: transport)
        await #expect(throws: RelayOutboxError.persistenceFailed) {
            try await outbox.submitNew(envelope(id))
        }
        #expect(await transport.postCount == 0)
    }

    @Test func twoOutboxInstancesCannotPostSameRequest() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("relay-outbox.json")
        let id = UUID().uuidString
        let sealed = try envelope(id)
        let transport = LostResponseTransport(journalFile: file, expectedID: id)
        let first = RelayOutbox(file: file, scope: scope, transport: transport)
        let second = RelayOutbox(file: file, scope: scope, transport: transport)

        async let firstResult = attempt(first, envelope: sealed)
        async let secondResult = attempt(second, envelope: sealed)
        let outcomes = await [firstResult, secondResult]
        #expect(outcomes.count == 2)
        #expect(outcomes.filter { if case .success(.uncertain) = $0 { true } else { false } }.count == 1)
        #expect(outcomes.filter {
            if case .failure(let error) = $0 { return error as? RelayOutboxError == .duplicateRequestID }
            return false
        }.count == 1)
        #expect(await transport.postCount == 1)
    }

    @Test func localRelayRejectionsNeverReserveAnID() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("relay-outbox.json")
        let id = UUID().uuidString
        let valid = try envelope(id)
        let transport = LostResponseTransport(journalFile: file, expectedID: id)
        let outbox = RelayOutbox(file: file, scope: scope, transport: transport)

        let wrongKind = EnvelopeV1(
            route: valid.route, sender: valid.sender, recipient: valid.recipient,
            requestID: id, kind: "response", expiresAtMilliseconds: valid.expiresAtMilliseconds,
            ephemeralKey: valid.ephemeralKey, nonce: valid.nonce,
            ciphertext: valid.ciphertext, signature: valid.signature)
        await #expect(throws: RelayOutboxError.invalidEnvelope) {
            try await outbox.submitNew(wrongKind)
        }
        let oversized = EnvelopeV1(
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
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("relay-outbox.json")
        let id = UUID().uuidString
        let transport = DelayedStatusTransport()
        let outbox = RelayOutbox(file: file, scope: scope, transport: transport)
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
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("relay-outbox.json")
        let id = UUID().uuidString
        let transport = DelayedPostTransport()
        let outbox = RelayOutbox(file: file, scope: scope, transport: transport)
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
}
