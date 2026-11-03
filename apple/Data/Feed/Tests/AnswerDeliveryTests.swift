@testable import AlondraFeed
import AlondraContract
import AlondraTransport
import AlondraTestSupport
import Foundation
import Testing

/// What one send meets on its way to the Mac and back.
private enum Leg: Sendable {
    /// Lost before the Mac: nothing ran.
    case lost
    /// The Mac ran it and its reply was lost.
    case replyLost
    case delivered
    /// The sealed bytes are too old for the relay and the Mac.
    case expired
}

/// A Mac behind an unreliable network. Like the daemon it records every answer by its ID and
/// replies to a repeat with what it already did; `applied` counts what really ran.
private actor UnreliableMac: ViewTransport {
    private var submits: [Leg]
    private var resends: [Leg]
    private var lookupsReachable: [Bool]
    private var records: [String: ActionResponse] = [:]
    private(set) var applied: [ActionRequest] = []
    private(set) var sent: [ActionRequest] = []
    private var refusal: (any Error)?
    private let onSend: @Sendable (Int) -> Void

    init(submits: [Leg] = [.delivered], resends: [Leg] = [], lookupsReachable: [Bool] = [],
         refusal: (any Error)? = nil, onSend: @escaping @Sendable (Int) -> Void = { _ in }) {
        self.submits = submits; self.resends = resends; self.lookupsReachable = lookupsReachable
        self.refusal = refusal; self.onSend = onSend
    }

    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch { throw FeedError.unknownCommand }
    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        throw FeedError.unknownCommand
    }
    func actionJournal(dataEpoch: String) async throws -> ActionJournalState? {
        try JSONDecoder().decode(ActionJournalState.self, from: Data(
            #"{"protocol":1,"data_epoch":"\#(dataEpoch)","generation":1,"max_pending":128,"max_ranges":129,"max_sequence":9007199254740991}"#.utf8))
    }
    func acknowledgeActions(_ acknowledgment: ActionJournalAcknowledgment) async throws {}

    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        try await travel(request, body: body, leg: submits.isEmpty ? .delivered : submits.removeFirst())
    }

    func resendAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse? {
        try await travel(request, body: body, leg: resends.isEmpty ? .delivered : resends.removeFirst())
    }

    func lookupAction(_ request: ActionRequest, body: Data) async throws -> Recorded<ActionResponse> {
        let reachable = lookupsReachable.isEmpty ? true : lookupsReachable.removeFirst()
        guard reachable else { throw URLError(.networkConnectionLost) }
        return records[request.commandId].map(Recorded.answered) ?? .absent
    }

    private func travel(_ request: ActionRequest, body: Data, leg: Leg) async throws -> ActionResponse {
        #expect(try ContractCodec.encode(request) == body)
        sent.append(request)
        onSend(sent.count)
        if let refusal { throw refusal }
        switch leg {
        case .lost: throw URLError(.networkConnectionLost)
        case .expired: throw OperationError.sealExpired
        case .replyLost:
            _ = try run(request)
            throw OperationError.outcomeUnknown
        case .delivered:
            return try run(request)
        }
    }

    private func run(_ request: ActionRequest) throws -> ActionResponse {
        if let recorded = records[request.commandId] { return recorded }
        applied.append(request)
        let response = try ContractCodec.decode(ActionResponse.self, from: Data(
            #"{"protocol":1,"outcome":"applied","stamp":"Answered","revision":2}"#.utf8))
        records[request.commandId] = response
        return response
    }
}

@Suite(.temporaryFiles) struct AnswerDeliveryTests {
    private let material = Data(repeating: 9, count: 32)
    private let scope = ActionIntentScope(service: "paired-mac", device: "phone", credentialRevision: "credential")
    private static let instant = ActionRetryPolicy(delay: { _ in .zero }, sleep: { _ in await Task.yield() })
    private static let untilWoken = ActionRetryPolicy(delay: { _ in .seconds(86_400) }, sleep: { try await Task.sleep(for: $0) })

    private func answer(_ nonce: String = "10000000-0000-4000-8000-000000000001") -> ActionRequest {
        ActionRequest(commandId: nonce, dataEpoch: "epoch", client: .ios, lang: "en",
            actionId: "ans:question-1@4", optionId: "yes", input: ActionInput(text: "Ship it"), resolvesCard: true)
    }

    private func store(_ directory: URL) async throws -> ViewActionIntentStore {
        try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope)
    }

    /// The store is written by deliveries and their cleanup; both end before the test's files go.
    private func settle(_ sender: ViewActionSender) async {
        await sender.drainDeliveries()
        await sender.drainRetirement()
    }

    @Test func theBackoffDoublesFromOneSecondUpToThirtyAndEachWaitStaysUnderIt() {
        #expect((1...8).map { ActionRetryPolicy.ceiling(attempt: $0) }
            == [1, 2, 4, 8, 16, 30, 30, 30].map { Duration.seconds($0) })
        for attempt in 1...8 {
            let delay = ActionRetryPolicy.live.delay(attempt)
            #expect(delay >= .milliseconds(250) && delay <= ActionRetryPolicy.ceiling(attempt: attempt))
        }
    }

    @Test func aLostReplyIsReadFromTheMacsRecordAndTheAnswerRunsOnce() async throws {
        let mac = UnreliableMac(submits: [.replyLost])
        let sender = ViewActionSender(transport: mac, intents: try await store(TestFiles.directory()), retryPolicy: Self.instant)
        let prepared = try await sender.prepare(answer())

        let response = try await sender.deliver(prepared)

        #expect(response.outcome == .applied)
        #expect(await mac.applied.map(\.commandId) == [prepared.commandId])
        #expect(await mac.sent.count == 1, "the record answered it; nothing went again")
        #expect(await sender.unresolvedCommandIDs().isEmpty)
        await settle(sender)
    }

    @Test func anAnswerLostOnTheWayGoesAgainAsTheSameCommandUntilItArrives() async throws {
        let mac = UnreliableMac(submits: [.lost], resends: [.lost, .lost, .delivered])
        let sender = ViewActionSender(transport: mac, intents: try await store(TestFiles.directory()), retryPolicy: Self.instant)
        let prepared = try await sender.prepare(answer())

        let response = try await sender.deliver(prepared)

        #expect(response.outcome == .applied)
        #expect(await mac.sent.map(\.commandId) == Array(repeating: prepared.commandId, count: 4))
        #expect(await mac.applied.count == 1)
        await settle(sender)
    }

    @Test func duplicatesThatReachTheMacAreAnsweredFromItsRecordNeverRunTwice() async throws {
        let mac = UnreliableMac(submits: [.replyLost], resends: [.replyLost, .delivered], lookupsReachable: [false, false])
        let sender = ViewActionSender(transport: mac, intents: try await store(TestFiles.directory()), retryPolicy: Self.instant)
        let prepared = try await sender.prepare(answer())

        let response = try await sender.deliver(prepared)

        #expect(response.outcome == .applied)
        #expect(await mac.sent.count == 3, "the Mac received the answer three times")
        #expect(await mac.applied.count == 1, "and ran it once")
        await settle(sender)
    }

    @Test func retryNowSendsAWaitingAnswerAtOnce() async throws {
        let firstTry = AsyncStream<Void>.makeStream()
        let mac = UnreliableMac(submits: [.lost], lookupsReachable: [false], onSend: { _ in firstTry.continuation.yield() })
        let sender = ViewActionSender(transport: mac, intents: try await store(TestFiles.directory()), retryPolicy: Self.untilWoken)
        let prepared = try await sender.prepare(answer())

        let delivery = Task { try await sender.deliver(prepared) }
        for await _ in firstTry.stream { break }
        await sender.retryNow()
        let response = try await delivery.value

        #expect(response.outcome == .applied)
        #expect(await mac.applied.count == 1)
        await settle(sender)
    }

    @Test func expiredBytesBecomeANewIDOnlyOnceTheMacSaysItNeverFinishedTheOld() async throws {
        let mac = UnreliableMac(submits: [.lost, .delivered], resends: [.expired, .expired], lookupsReachable: [false, true])
        let sender = ViewActionSender(transport: mac, intents: try await store(TestFiles.directory()), retryPolicy: Self.instant)
        let prepared = try await sender.prepare(answer())

        let response = try await sender.deliver(prepared)

        #expect(response.outcome == .applied)
        let sent = await mac.sent.map(\.commandId)
        #expect(sent.count == 4)
        #expect(Array(sent.prefix(3)) == Array(repeating: prepared.commandId, count: 3),
                "while the Mac could not be asked, the old ID is never replaced")
        let old = try #require(ActionOperationID(rawValue: prepared.commandId))
        let last = try #require(sent.last)
        let new = try #require(ActionOperationID(rawValue: last))
        #expect(new.generation == old.generation && new.sequence == old.sequence + 1 && new.nonce == old.nonce)
        #expect(await mac.applied.map(\.commandId) == [new.rawValue])
        #expect(await mac.applied.first?.input == prepared.input)
        #expect(await sender.unresolvedCommandIDs().isEmpty)
        await settle(sender)
    }

    @Test func aConflictIsRetriedByItsKindNeverByTheWordsBesideIt() {
        func disposition(_ body: String) -> ActionDeliveryDisposition {
            ActionDeliveryDisposition(APIError.from(status: 409, body: Data(body.utf8)))
        }
        #expect(disposition(#"{"error":"The Mac could not tell","kind":"outcome_unknown"}"#) == .retry)
        #expect(disposition(#"{"error":"outcome_unknown"}"#) == .retry, "a daemon from before kind sends the code alone")
        #expect(disposition(#"{"error":"outcome_unknown","kind":"operation_conflict"}"#) == .final)
        #expect(disposition(#"{"error":"The command conflicts with another"}"#) == .final)
    }

    @Test func aRefusalFromTheMacEndsTheDeliveryAndLeavesNothingQueued() async throws {
        let mac = UnreliableMac(refusal: APIError.forbidden(kind: "device_revoked", detail: nil, reasons: []))
        let sender = ViewActionSender(transport: mac, intents: try await store(TestFiles.directory()), retryPolicy: Self.instant)
        let prepared = try await sender.prepare(answer())

        await #expect(throws: APIError.forbidden(kind: "device_revoked", detail: nil, reasons: [])) {
            try await sender.deliver(prepared)
        }
        #expect(await mac.sent.count == 1)
        #expect(await sender.unresolvedCommandIDs().isEmpty)
        #expect(sender.undelivered.current.isEmpty)
        await settle(sender)
    }

    @Test func anAnswerForDataTheMacResetCanNeverBeDelivered() async throws {
        let mac = UnreliableMac(submits: [.lost], lookupsReachable: [false])
        let sender = ViewActionSender(transport: mac, intents: try await store(TestFiles.directory()), retryPolicy: Self.instant)
        let prepared = try await sender.prepare(answer())
        await sender.resumeDeliveries(dataEpoch: "another-epoch")

        await #expect(throws: ActionDeliveryError.dataReset) { try await sender.deliver(prepared) }
        #expect(await mac.applied.isEmpty)
        await settle(sender)
    }

    @Test func anAnswerLeftByAnEarlierLaunchIsDeliveredOnItsOwnAndItsCardStaysOutUntilThen() async throws {
        let directory = try TestFiles.directory()
        let gone = UnreliableMac(submits: [.lost], lookupsReachable: [false])
        let first = ViewActionSender(transport: gone, intents: try await store(directory), retryPolicy: Self.untilWoken)
        let prepared = try await first.prepare(answer())
        let tried = Task { try await first.deliver(prepared) }
        while await gone.sent.isEmpty { await Task.yield() }
        await first.stop()
        _ = await tried.result
        #expect(first.undelivered.current == ["question-1"])

        let mac = UnreliableMac()
        let relaunched = ViewActionSender(transport: mac, intents: try await store(directory), retryPolicy: Self.instant)
        #expect(relaunched.undelivered.current == ["question-1"], "the queue drawn after the relaunch keeps the card out")
        await relaunched.resumeDeliveries(dataEpoch: "epoch")
        await relaunched.drainDeliveries()

        #expect(await mac.applied.map(\.commandId) == [prepared.commandId])
        #expect(await relaunched.unresolvedCommandIDs().isEmpty)
        #expect(relaunched.undelivered.current.isEmpty)
        await settle(first)
        await settle(relaunched)
    }
}
