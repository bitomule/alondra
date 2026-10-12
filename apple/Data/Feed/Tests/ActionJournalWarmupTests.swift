@testable import AlondraFeed
import AlondraContract
import AlondraTransport
import AlondraTestSupport
import Foundation
import Synchronization
import Testing

private enum WarmupProgress: Sendable { case descriptor, acknowledgment, finished }

private actor WarmupJournalPeer: ViewTransport {
    private let holdDescriptor: Bool
    private let forbidDescriptor: Bool
    private let progress: @Sendable (WarmupProgress) -> Void
    private var descriptorAvailable = true
    private var released = false
    private var held: CheckedContinuation<Void, Never>?

    init(holdDescriptor: Bool = false, forbidDescriptor: Bool = false,
         progress: @escaping @Sendable (WarmupProgress) -> Void = { _ in }) {
        self.holdDescriptor = holdDescriptor; self.forbidDescriptor = forbidDescriptor
        self.progress = progress
    }
    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        throw FeedError.unknownCommand
    }
    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        throw FeedError.unknownCommand
    }
    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        Issue.record("warming the descriptor cannot POST an action")
        throw CancellationError()
    }
    func actionJournal(dataEpoch: String) async throws -> ActionJournalState? {
        if forbidDescriptor {
            Issue.record("an obsolete owner cannot start a descriptor request")
            throw CancellationError()
        }
        guard descriptorAvailable else { throw URLError(.networkConnectionLost) }
        #expect(dataEpoch == "epoch")
        if holdDescriptor && !released {
            await withCheckedContinuation { continuation in
                held = continuation
                progress(.descriptor)
            }
        } else { progress(.descriptor) }
        return try JSONDecoder().decode(ActionJournalState.self, from: Data(#"{"protocol":1,"data_epoch":"epoch","generation":1,"max_pending":128,"max_ranges":129,"max_sequence":9007199254740991}"#.utf8))
    }
    func acknowledgeActions(_ acknowledgment: ActionJournalAcknowledgment) async throws {
        progress(.acknowledgment)
        Issue.record("a recovery that lost its owner cannot start cleanup after descriptor hydration")
        throw CancellationError()
    }
    func makeDescriptorUnavailable() { descriptorAvailable = false }
    func releaseDescriptor() {
        released = true
        let waiting = held; held = nil
        waiting?.resume()
    }
}

@Suite(.temporaryFiles) struct ActionJournalWarmupTests {
    private func request() -> ActionRequest {
        ActionRequest(commandId: "40000000-0000-4000-8000-000000000001", dataEpoch: "epoch",
            client: .ios, lang: "en", actionId: "reply:fixture@1", input: ActionInput(text: "Saved draft"))
    }
    private func store() async throws -> ViewActionIntentStore {
        let directory = try TestFiles.directory()
        return try await ViewActionIntentStore(directory: directory, keyMaterial: Data(repeating: 31, count: 32),
            scope: .init(service: "fixture", device: "phone", credentialRevision: "approval"))
    }

    @Test func anEmptyRestorationPreparesItsFirstActionAfterTheDescriptorBecomesUnavailable() async throws {
        let peer = WarmupJournalPeer()
        let sender = ViewActionSender(transport: peer, intents: try await store())
        await sender.reconcilePending(dataEpoch: "epoch")
        await peer.makeDescriptorUnavailable()
        let prepared = try await sender.prepare(request())
        await sender.drainRetirement()
        let identity = try #require(ActionOperationID(rawValue: prepared.commandId))
        #expect(identity.generation == 1)
        #expect(prepared.input.text == "Saved draft")
        #expect(await sender.unresolvedRequests() == [prepared])
    }

    @Test func anAlreadyObsoleteRestorationDoesNotStartDescriptorWork() async throws {
        let peer = WarmupJournalPeer(forbidDescriptor: true)
        let sender = ViewActionSender(transport: peer, intents: try await store())
        await sender.reconcilePending(dataEpoch: "epoch", isCurrent: { false })
        await sender.drainRetirement()
        #expect(await sender.unresolvedRequests().isEmpty)
    }

    @Test func ownershipLostWhileTheDescriptorIsHeldCannotStartACKWork() async throws {
        let progress = AsyncStream<WarmupProgress>.makeStream()
        let peer = WarmupJournalPeer(holdDescriptor: true, progress: { progress.continuation.yield($0) })
        let intents = try await store()
        let record = try await intents.prepareNew(request(), generation: 1)
        let response = try ContractCodec.decode(ActionResponse.self,
            from: Data(#"{"protocol":1,"outcome":"applied","stamp":"Saved","revision":2}"#.utf8))
        try await intents.complete(record, response: response)
        let sender = ViewActionSender(transport: peer, intents: intents)
        let current = Mutex(true)
        let recovery = Task {
            await sender.reconcilePending(dataEpoch: "epoch", isCurrent: { current.withLock { $0 } })
            progress.continuation.yield(.finished)
        }
        var events = progress.stream.makeAsyncIterator()
        let first = await events.next()
        if case .descriptor? = first {
            current.withLock { $0 = false }
        } else {
            Issue.record("restoration must hydrate its descriptor before considering completed cleanup")
        }
        await peer.releaseDescriptor()
        await recovery.value
        await sender.drainRetirement()
        progress.continuation.finish()
        #expect(await sender.unresolvedRequests().isEmpty)
        let retained = try await intents.reserve(record.restoredRequest, body: record.body)
        if case let .completed(saved) = retained { #expect(saved == response) }
        else { Issue.record("losing the recovery owner cannot discard a committed local outcome") }
    }
}
