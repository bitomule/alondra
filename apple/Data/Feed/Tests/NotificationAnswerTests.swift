@testable import AlondraFeed
import AlondraContract
import AlondraTransport
import AlondraTestSupport
import Foundation
import Testing

/// A Mac a notification button tries to reach from the background: out of reach until the app is
/// opened, then answering. Each action runs once however often it arrives.
private actor DistantMac: ViewTransport {
    private var reachable = false
    private(set) var runs: [String: Int] = [:]

    func comeIntoReach() { reachable = true }

    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        throw FeedError.unknownCommand
    }
    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        throw FeedError.unknownCommand
    }
    func actionJournal(dataEpoch: String) async throws -> ActionJournalState? {
        guard reachable else { throw URLError(.cannotConnectToHost) }
        return try JSONDecoder().decode(ActionJournalState.self, from: Data(#"{"protocol":1,"data_epoch":"epoch","generation":1,"max_pending":128,"max_ranges":129,"max_sequence":9007199254740991}"#.utf8))
    }
    func acknowledgeActions(_ acknowledgment: ActionJournalAcknowledgment) async throws {}
    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        try run(request)
    }
    func lookupAction(_ request: ActionRequest, body: Data) async throws -> Recorded<ActionResponse> {
        guard reachable else { throw URLError(.cannotConnectToHost) }
        return runs[request.commandId] == nil ? .absent : .answered(try applied())
    }
    func resendAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse? {
        try run(request)
    }

    private func run(_ request: ActionRequest) throws -> ActionResponse {
        guard reachable else { throw URLError(.cannotConnectToHost) }
        runs[request.commandId, default: 0] += 1
        return try applied()
    }

    private func applied() throws -> ActionResponse {
        try ContractCodec.decode(ActionResponse.self, from: Data(#"{"protocol":1,"outcome":"applied","stamp":"Saved","revision":2}"#.utf8))
    }
}

@Suite(.temporaryFiles) struct NotificationAnswerTests {
    private func answer() -> ActionRequest {
        ActionRequest(commandId: "40000000-0000-4000-8000-000000000009", dataEpoch: "epoch",
            client: .ios, lang: "en", actionId: "reply:fixture@1", input: ActionInput(text: "Ship it"))
    }

    private func store(knowingGeneration: Bool) async throws -> ViewActionIntentStore {
        let store = try await ViewActionIntentStore(directory: try TestFiles.directory(), keyMaterial: Data(repeating: 31, count: 32),
            scope: .init(service: "fixture", device: "phone", credentialRevision: "approval"))
        if knowingGeneration {
            let earlier = ActionRequest(commandId: "40000000-0000-4000-8000-000000000001", dataEpoch: "epoch",
                client: .ios, lang: "en", actionId: "reply:fixture@1", input: ActionInput(text: "Earlier"))
            let record = try await store.prepareNew(earlier, generation: 1)
            try await store.complete(record, response: ContractCodec.decode(ActionResponse.self,
                from: Data(#"{"protocol":1,"outcome":"applied","stamp":"Saved","revision":1}"#.utf8)))
        }
        return store
    }

    /// 16 Oct: a button pressed with the phone locked and the Mac out of reach says it could not
    /// send, and the answer goes by itself once the app is open.
    @Test func anAnswerTheMacCouldNotTakeIsKeptAndGoesOnceTheAppIsOpen() async throws {
        let mac = DistantMac()
        let sender = ViewActionSender(transport: mac, intents: try await store(knowingGeneration: true))
        await #expect(throws: AnswerKeptForLater()) { try await sender.sendNow(answer()) }
        let kept = try #require(await sender.unresolvedRequests().first)
        #expect(ActionOperationID(rawValue: kept.commandId)?.generation == 1)
        #expect(kept.input.text == "Ship it")

        await mac.comeIntoReach()
        await sender.resumeDeliveries(dataEpoch: "epoch")
        await sender.drainDeliveries()
        #expect(await sender.unresolvedRequests().isEmpty)
        #expect(await mac.runs == [kept.commandId: 1])
        await sender.drainRetirement()
    }

    @Test func anAnswerTheMacTakesAtOnceIsSentAtOnce() async throws {
        let mac = DistantMac()
        await mac.comeIntoReach()
        let sender = ViewActionSender(transport: mac, intents: try await store(knowingGeneration: true))
        #expect(try await sender.sendNow(answer()).outcome == .applied)
        #expect(await sender.unresolvedRequests().isEmpty)
        #expect(await mac.runs.count == 1)
        await sender.drainRetirement()
    }

    /// Without the Mac's journal generation the answer cannot be numbered, so it is not kept and
    /// the person is told it was not sent.
    @Test func withoutAKnownJournalNothingIsKeptAndTheFailureShows() async throws {
        let mac = DistantMac()
        let sender = ViewActionSender(transport: mac, intents: try await store(knowingGeneration: false))
        await #expect(throws: AnswerNotSent()) { try await sender.sendNow(answer()) }
        #expect(await sender.unresolvedRequests().isEmpty)
        #expect(await mac.runs.isEmpty)
        await sender.drainRetirement()
    }
}
