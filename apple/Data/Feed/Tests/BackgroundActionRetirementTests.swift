@testable import AlondraFeed
import AlondraContract
import AlondraTransport
import AlondraTestSupport
import Foundation
import Testing

private actor BackgroundRetirementPeer: ViewTransport {
    private let holdFirst: Bool
    private let failFirst: Bool
    private let response: ActionResponse
    private var ackUnavailable: Bool
    private var serverOwned: Set<String> = []
    private var released = false
    private var entryWaiters: [CheckedContinuation<Void, Never>] = []
    private var releaseWaiters: [CheckedContinuation<Void, Never>] = []
    private(set) var acknowledgments: [ActionJournalAcknowledgment] = []
    private(set) var posts: [ActionRequest] = []
    private var activeACKs = 0
    private(set) var peakACKs = 0
    init(holdFirst: Bool = false, failFirst: Bool = false, ackUnavailable: Bool = false) throws {
        self.holdFirst = holdFirst; self.failFirst = failFirst; self.ackUnavailable = ackUnavailable
        response = try ContractCodec.decode(ActionResponse.self,
            from: Data(#"{"protocol":1,"outcome":"applied","stamp":"Saved","revision":2}"#.utf8))
    }
    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch { throw FeedError.unknownCommand }
    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed { throw FeedError.unknownCommand }
    func actionJournal(dataEpoch: String) async throws -> ActionJournalState? {
        try JSONDecoder().decode(ActionJournalState.self, from: Data(#"{"protocol":1,"data_epoch":"epoch","generation":1,"max_pending":128,"max_ranges":129,"max_sequence":9007199254740991}"#.utf8))
    }
    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        #expect(try ContractCodec.encode(request) == body)
        posts.append(request)
        guard serverOwned.count < 128 else { throw OperationError.outcomeUnknown }
        serverOwned.insert(request.commandId)
        return response
    }
    func acknowledgeActions(_ acknowledgment: ActionJournalAcknowledgment) async throws {
        acknowledgments.append(acknowledgment)
        let ordinal = acknowledgments.count
        activeACKs += 1; peakACKs = max(peakACKs, activeACKs)
        defer { activeACKs -= 1 }
        for waiter in entryWaiters { waiter.resume() }; entryWaiters.removeAll()
        if ordinal == 1 && holdFirst && !released {
            await withCheckedContinuation { releaseWaiters.append($0) }
        }
        if ackUnavailable || (ordinal == 1 && failFirst) { throw OperationError.outcomeUnknown }
        serverOwned = serverOwned.filter { id in
            guard let ticket = ActionOperationID(rawValue: id), ticket.generation == acknowledgment.generation else { return true }
            return !acknowledgment.ranges.contains { $0.first <= ticket.sequence && ticket.sequence <= $0.last }
        }
    }
    func restoreAcknowledgments() { ackUnavailable = false }
    func entered() async {
        if acknowledgments.isEmpty { await withCheckedContinuation { entryWaiters.append($0) } }
    }
    func release() {
        released = true
        for waiter in releaseWaiters { waiter.resume() }; releaseWaiters.removeAll()
    }
}

@Suite(.temporaryFiles) struct BackgroundActionRetirementTests {
    private func request(_ ordinal: Int) -> ActionRequest {
        ActionRequest(commandId: String(format: "30000000-0000-4000-8000-%012x", ordinal),
            dataEpoch: "epoch", client: .ios, lang: "en", actionId: "answer:\(ordinal)")
    }
    private func sender(_ peer: BackgroundRetirementPeer, directory: URL? = nil) async throws -> ViewActionSender {
        let root = try directory ?? TestFiles.directory()
        let intents = try await ViewActionIntentStore(directory: root,
            keyMaterial: Data(repeating: 29, count: 32), scope: .init(service: "peer", device: "phone", credentialRevision: "approval"))
        return ViewActionSender(transport: peer, intents: intents)
    }

    @Test func responsesReturnWhileOneOwnedACKIsHeldAndLaterCompletionsCoalesce() async throws {
        let peer = try BackgroundRetirementPeer(holdFirst: true)
        let sender = try await sender(peer)
        do {
            let first = try await sender.prepare(request(1))
            let response = try await sender.send(first)
            #expect(response.stamp == "Saved", "cleanup cannot hold the applied response")
            await peer.entered()
            #expect(await peer.acknowledgments.count == 1)
            for ordinal in 2...3 {
                let ticket = try await sender.prepare(request(ordinal))
                #expect(try await sender.send(ticket).outcome == .applied)
            }
            #expect(await peer.posts.count == 3)
            #expect(await peer.acknowledgments.count == 1, "a held cleanup owns the only physical ACK task")
            await peer.release()
            await sender.drainRetirement()
            let acknowledgments = await peer.acknowledgments
            #expect(acknowledgments.map(\.ranges) == [[.init(first: 1, last: 1)], [.init(first: 1, last: 3)]])
            #expect(await peer.peakACKs == 1)
            #expect(await sender.retirementNeedsRetry() == false)
        } catch {
            await peer.release()
            await sender.drainRetirement()
            throw error
        }
    }

    @Test func failedACKKeepsTheAppliedResultAndReconciliationRetriesOnlyCleanup() async throws {
        let peer = try BackgroundRetirementPeer(failFirst: true)
        let sender = try await sender(peer)
        let prepared = try await sender.prepare(request(1))
        #expect(try await sender.send(prepared).outcome == .applied)
        await sender.drainRetirement()
        #expect(await sender.retirementNeedsRetry())
        #expect(await peer.posts.count == 1)
        await sender.reconcilePending(dataEpoch: "epoch")
        await sender.drainRetirement()
        #expect(await sender.retirementNeedsRetry() == false)
        let acknowledgments = await peer.acknowledgments
        #expect(acknowledgments.map(\.ranges) == [[.init(first: 1, last: 1)], [.init(first: 1, last: 1)]])
        #expect(await peer.posts.count == 1)
        #expect(await sender.unresolvedCommandIDs().isEmpty)
        #expect(try await sender.send(prepared).outcome == .applied)
        await sender.drainRetirement()
        #expect(await peer.posts.count == 1)
    }
    @Test func ACKOutageStopsAction129BeforePOSTAndRecoveryAdmitsThatSameDraftOnce() async throws {
        let directory = try TestFiles.directory()
        let peer = try BackgroundRetirementPeer(ackUnavailable: true)
        let sender = try await sender(peer, directory: directory)
        for ordinal in 1...128 {
            let prepared = try await sender.prepare(request(ordinal))
            #expect(try await sender.send(prepared).outcome == .applied)
            await sender.drainRetirement()
        }
        #expect(await sender.retirementNeedsRetry())
        await #expect(throws: ViewActionIntentError.capacityExceeded) { try await sender.prepare(request(129)) }
        #expect(await sender.unresolvedCommandIDs().isEmpty)
        #expect(await peer.posts.count == 128, "the rejected candidate must not reach the server's full window")
        await peer.restoreAcknowledgments()
        let reopened = try await self.sender(peer, directory: directory)
        let prepared = try await reopened.prepare(request(129))
        #expect(prepared.commandId.contains("_1_129_"))
        #expect(try await reopened.send(prepared).outcome == .applied)
        await reopened.drainRetirement()
        #expect(await peer.posts.count == 129)
        #expect(await peer.posts.last?.commandId == prepared.commandId)
        let anotherProcess = try await self.sender(peer, directory: directory)
        let next = try await anotherProcess.prepare(request(130))
        #expect(next.commandId.contains("_1_130_"))
        #expect(try await anotherProcess.send(next).outcome == .applied)
        await anotherProcess.drainRetirement()
        #expect(await peer.posts.count == 130)
    }

}
