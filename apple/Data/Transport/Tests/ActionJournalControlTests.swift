@testable import AlondraTransport
import AlondraTestSupport
import CryptoKit
import Foundation
import Testing

private actor JournalControlPeer: OperationAPI {
    let journal: EnvelopeJournal?
    var replies: [APIResponse]
    private(set) var retirements: [ActionJournalAcknowledgment] = []
    private(set) var requests: [APIRequest] = []
    init(journal: EnvelopeJournal? = nil, replies: [APIResponse]) { self.journal = journal; self.replies = replies }
    func perform(_ request: APIRequest) async throws -> APIResponse {
        requests.append(request)
        return replies.removeFirst()
    }
    func retireActions(_ acknowledgment: ActionJournalAcknowledgment) async throws {
        try await journal?.retire(acknowledgment)
        retirements.append(acknowledgment)
    }
}

@Suite(.temporaryFiles) struct ActionJournalControlTests {
    private let scope = RelayOutboxScope(relay: URL(string: "https://relay.example.test")!,
        macID: "mac", deviceID: "phone", credentialRevision: "approval")
    private func ticket(_ generation: UInt64, _ sequence: UInt64) throws -> String {
        try #require(ActionOperationID(dataEpoch: "epoch", generation: generation, sequence: sequence,
            nonce: "00000000-0000-4000-8000-000000000001")).rawValue
    }
    private func response(_ extra: String = "", generation: UInt64 = 7) -> APIResponse {
        .completed(status: 200, body: Data("{\"protocol\":1,\"data_epoch\":\"epoch\",\"generation\":\(generation),\"max_pending\":128,\"max_ranges\":129,\"max_sequence\":9007199254740991\(extra)}".utf8))
    }
    private func api(_ peer: JournalControlPeer) -> AlondraAPIClient {
        AlondraAPIClient(serviceURL: URL(string: "https://service.example.test")!, deviceID: "phone", operations: peer)
    }

    @Test func verifiedGenerationFencesOldTransportPayloadsBeforeAnyCurrentActionCompletes() async throws {
        let directory = try TestFiles.directory()
        let journal = try EnvelopeJournal(scope: scope, directory: directory)
        let old = try ticket(6, 1), current = try ticket(7, 1)
        try await journal.reserve(.init(requestID: old, scope: scope, envelope: nil, phase: .uncertain))
        try await journal.reserve(.init(requestID: current, scope: scope, envelope: nil, phase: .uncertain))
        let peer = JournalControlPeer(journal: journal, replies: [response()])
        _ = try await api(peer).actionJournal(dataEpoch: "epoch")
        #expect(try await journal.entries().map(\.requestID) == [current])
        let reopened = try EnvelopeJournal(scope: scope, directory: directory)
        #expect(try await reopened.entry(requestID: old)?.phase == .retired)
        #expect(try await reopened.entry(requestID: current)?.phase == .uncertain)
        await #expect(throws: EnvelopeJournalError.duplicateRequestID) {
            try await reopened.reserve(.init(requestID: old, scope: scope, envelope: nil, phase: .reserved))
        }
        #expect(await peer.retirements.count == 1)
    }

    @Test func anAcknowledgmentWithoutExactRangeCoverageCannotDeleteTransportProofs() async throws {
        let peer = JournalControlPeer(replies: [response(",\"acknowledged\":true,\"ranges\":[{\"first\":2,\"last\":3}]"),
            response(",\"acknowledged\":true,\"ranges\":[{\"first\":1,\"last\":4}]")])
        let ack = ActionJournalAcknowledgment(dataEpoch: "epoch", generation: 7, ranges: [.init(first: 1, last: 3)])
        await #expect(throws: OperationError.outcomeUnknown) { try await api(peer).acknowledgeActions(ack) }
        #expect(await peer.retirements.isEmpty)
        try await api(peer).acknowledgeActions(ack)
        #expect(await peer.retirements.count == 1)
        #expect(await peer.requests.allSatisfy(\.isActionJournalControl))
    }

    @Test func delayedOrEmptyRetirementMessagesNeverForgetCompletedRanges() throws {
        let full = ActionJournalAcknowledgment(dataEpoch: "epoch", generation: 7, ranges: [.init(first: 2, last: 1_000_000)])
        let empty = ActionJournalAcknowledgment(dataEpoch: "epoch", generation: 7, ranges: [])
        let merged = try empty.merging(full)
        #expect(merged.ranges == full.ranges)
        #expect(merged.retires(try ticket(7, 999_999)))
        #expect(!merged.retires(try ticket(7, 1)))
        #expect(try ActionJournalAcknowledgment(dataEpoch: "epoch", generation: 6, ranges: []).merging(full).generation == 7)
    }
}

private actor SuccessfulJournalHTTP: HTTPTransport {
    func send(_ request: URLRequest) async throws -> HTTPReply { HTTPReply(status: 200, body: Data("{}".utf8)) }
}

extension ActionJournalControlTests {
    @Test func loopbackAcknowledgmentsDoNotCreateOneMemoryOutcomePerCleanup() async throws {
        let key = P256.Signing.PrivateKey()
        let identity = LoopbackIdentityStore(directory: try TestFiles.directory(), makeKey: { .software(key) })
        _ = try await identity.signingKey()
        try await identity.save(deviceID: "fixture-client")
        let session = LoopbackSession(identity: identity, deviceName: "Fixture", requestClock: .continuous,
            transport: SuccessfulJournalHTTP())
        let request = try APIRequest(endpoint: Endpoint<Data>.data(.post, path: ["operation-journal", "ack"],
            body: Data("{}".utf8)), operationID: "control-fixture")
        #expect(try await session.perform(request) == .completed(status: 200, body: Data("{}".utf8)))
        #expect(await session.outcome(requestID: request.operationID) == .unknown)
    }

    @Test func compactedEvidenceReleasesIdleEntriesButRetainsALiveLeaseUntilItEnds() throws {
        let store = MutationEvidenceStore()
        let id = try ticket(7, 1)
        let lease = store.acquire(id, owner: UUID())
        store.retain(id)
        store.retireActions(.init(dataEpoch: "epoch", generation: 7, ranges: [.init(first: 1, last: 1)]))
        #expect(store.entryCount == 1)
        lease.evidence.release(by: lease.ownerID)
        store.release(lease)
        #expect(store.entryCount == 0)
    }
}
