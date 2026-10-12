@testable import AlondraFeed
import AlondraContract
import AlondraTransport
import AlondraTestSupport
import Foundation
import Testing
import Synchronization

private let originalIntentID = "00000000-0000-4000-8000-000000000001"
private let replacementIntentID = "00000000-0000-4000-8000-000000000002"

private actor IntentPeer: ViewTransport {
    private(set) var posts: [ActionRequest] = []
    private(set) var recoveries: [ActionRequest] = []
    private let result: ActionResponse
    private let answerRecovery: Bool
    init(answerRecovery: Bool = true) throws {
        self.answerRecovery = answerRecovery
        result = try ContractCodec.decode(ActionResponse.self,
            from: Data(#"{"protocol":1,"outcome":"applied","stamp":"Saved original answer","revision":2}"#.utf8))
    }
    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch { throw FeedError.unknownCommand }
    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed { throw FeedError.unknownCommand }
    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        #expect(try ContractCodec.encode(request) == body)
        posts.append(request)
        throw OperationError.outcomeUnknown
    }
    func recoverAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse? {
        #expect(try ContractCodec.encode(request) == body)
        recoveries.append(request)
        return answerRecovery ? result : nil
    }
}

@Suite(.temporaryFiles) struct DurableActionIntentTests {
    private let material = Data(repeating: 7, count: 32)
    private let scope = ActionIntentScope(service: "paired-mac", device: "phone", credentialRevision: "credential")
    private func request(_ id: String = originalIntentID, epoch: String = "epoch", lang: String = "en", resolves: Bool = true) -> ActionRequest {
        ActionRequest(commandId: id, dataEpoch: epoch, client: .ios, lang: lang,
            actionId: "ans:question@1", optionId: "yes", input: ActionInput(text: "Private original answer"), resolvesCard: resolves)
    }

    @Test func relaunchRecoversTheOriginalIntentWhenTheScreenCreatesAnotherUUID() async throws {
        let directory = try TestFiles.directory()
        let peer = try IntentPeer()
        let originalStore = try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope)
        let original = ViewActionSender(transport: peer, intents: originalStore)
        await #expect(throws: OperationError.outcomeUnknown) { try await original.send(request()) }
        #expect(await peer.posts.map(\.commandId) == [originalIntentID])
        let ciphertext = try Data(contentsOf: #require(try FileManager.default.contentsOfDirectory(at: directory,
            includingPropertiesForKeys: nil).first(where: { $0.pathExtension == "intents" })))
        #expect(ciphertext.range(of: Data("Private original answer".utf8)) == nil)

        let reopened = try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope)
        let restored = ViewActionSender(transport: peer, intents: reopened)
        #expect(await restored.unresolvedCommandIDs() == [originalIntentID])
        #expect(await restored.unresolvedRequests().first?.resolvesItsCard == true)
        #expect(await peer.recoveries.isEmpty, "restore does not automatically send or recover anything")
        let reply = try await restored.send(request(replacementIntentID, lang: "es", resolves: false))
        #expect(reply.stamp == "Saved original answer")
        #expect(await peer.posts.map(\.commandId) == [originalIntentID])
        #expect(await peer.recoveries.map(\.commandId) == [originalIntentID])
        #expect(await peer.recoveries.first?.resolvesItsCard == true)
        #expect(await restored.unresolvedCommandIDs().isEmpty)
        let afterRetirement = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope))
        #expect(await afterRetirement.unresolvedCommandIDs().isEmpty)
    }

    @Test func unavailableRecoveryRetainsTheIntentWithoutAnotherPOST() async throws {
        let directory = try TestFiles.directory()
        let peer = try IntentPeer(answerRecovery: false)
        let first = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope))
        await #expect(throws: OperationError.outcomeUnknown) { try await first.send(request()) }
        let restored = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope))
        await #expect(throws: OperationError.outcomeUnknown) { try await restored.send(request(replacementIntentID)) }
        await #expect(throws: OperationError.outcomeUnknown) { try await restored.retry(commandID: originalIntentID) }
        #expect(await peer.posts.map(\.commandId) == [originalIntentID])
        #expect(await peer.recoveries.map(\.commandId) == [originalIntentID, originalIntentID])
        #expect(await restored.unresolvedCommandIDs() == [originalIntentID])
        let reopened = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope))
        #expect(await reopened.unresolvedRequests() == [request()])
    }

    @Test func anOldEpochDoesNotReplaceAnExplicitIntentForTheNewEpoch() async throws {
        let directory = try TestFiles.directory()
        let peer = try IntentPeer()
        let first = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope))
        await #expect(throws: OperationError.outcomeUnknown) { try await first.send(request()) }
        let restored = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope))
        await #expect(throws: OperationError.outcomeUnknown) {
            try await restored.send(request(replacementIntentID, epoch: "new-epoch"))
        }
        #expect(await peer.posts.map(\.commandId) == [originalIntentID, replacementIntentID])
        #expect(await peer.recoveries.isEmpty)
        #expect(await restored.unresolvedCommandIDs() == [originalIntentID, replacementIntentID])
    }

    @Test func changedCredentialsCannotReadOrRecoverAnotherIdentitiesIntent() async throws {
        let directory = try TestFiles.directory()
        let peer = try IntentPeer()
        let first = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope))
        await #expect(throws: OperationError.outcomeUnknown) { try await first.send(request()) }
        let otherScope = ActionIntentScope(service: scope.service, device: "other-phone", credentialRevision: "new-credential")
        let other = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: Data(repeating: 8, count: 32), scope: otherScope))
        #expect(await other.unresolvedCommandIDs().isEmpty)
        await #expect(throws: FeedError.unknownCommand) { try await other.retry(commandID: originalIntentID) }
        #expect(await peer.recoveries.isEmpty)
        #expect(await peer.posts.count == 1)
        await #expect(throws: ViewActionIntentError.unreadableStore) {
            try await ViewActionIntentStore(directory: directory, keyMaterial: Data(repeating: 8, count: 32), scope: scope)
        }
    }

    @Test func deletingAnInitializedStoreCannotAuthorizeAnotherPOST() async throws {
        let directory = try TestFiles.directory()
        let peer = try IntentPeer()
        let store = try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope)
        let sender = ViewActionSender(transport: peer, intents: store)
        let file = try #require(try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)
            .first(where: { $0.pathExtension == "intents" }))
        try FileManager.default.removeItem(at: file)
        await #expect(throws: ViewActionIntentError.unreadableStore) { try await sender.send(request()) }
        #expect(await peer.posts.isEmpty)
        await #expect(throws: ViewActionIntentError.unreadableStore) {
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope)
        }
        #expect(!FileManager.default.fileExists(atPath: file.path))
    }

    @Test func independentlyOpenedStoresPreserveBothPendingIntents() async throws {
        let directory = try TestFiles.directory()
        let peer = try IntentPeer()
        let first = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope))
        let second = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope))
        await #expect(throws: OperationError.outcomeUnknown) { try await first.send(request()) }
        await #expect(throws: OperationError.outcomeUnknown) {
            try await second.send(request(replacementIntentID, epoch: "other-epoch"))
        }
        let restored = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope))
        #expect(await restored.unresolvedCommandIDs() == [originalIntentID, replacementIntentID])
        #expect(await peer.posts.map(\.commandId) == [originalIntentID, replacementIntentID])
    }

    @Test func independentlyOpenedSendersCannotPOSTTheSamePendingIntentTwice() async throws {
        let directory = try TestFiles.directory()
        let peer = try IntentPeer()
        let first = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope))
        let second = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope))
        await #expect(throws: OperationError.outcomeUnknown) { try await first.send(request()) }
        #expect(try await second.send(request(replacementIntentID)).outcome == .applied)
        #expect(await peer.posts.map(\.commandId) == [originalIntentID])
        #expect(await peer.recoveries.map(\.commandId) == [originalIntentID])
        let restored = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope))
        #expect(try await restored.send(request()).outcome == .applied)
        #expect(await peer.posts.count == 1)
        #expect(await peer.recoveries.count == 1, "a verified durable completion survives presentation/relaunch without another network request")
    }

    @Test func capacityReservesRoomForEveryAdmittedPendingCompletion() async throws {
        let directory = try TestFiles.directory()
        let peer = try IntentPeer()
        let store = try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope, completedLimit: 2)
        let sender = ViewActionSender(transport: peer, intents: store)
        await #expect(throws: OperationError.outcomeUnknown) { try await sender.send(request()) }
        _ = try await sender.retry(commandID: originalIntentID)
        await #expect(throws: OperationError.outcomeUnknown) {
            try await sender.send(request(replacementIntentID, epoch: "second-epoch"))
        }
        await #expect(throws: ViewActionIntentError.capacityExceeded) {
            try await sender.send(request("00000000-0000-4000-8000-000000000003", epoch: "third-epoch"))
        }
        #expect(await peer.posts.map(\.commandId) == [originalIntentID, replacementIntentID])
        _ = try await sender.retry(commandID: replacementIntentID)
        let reopened = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope, completedLimit: 2))
        #expect(await reopened.unresolvedCommandIDs().isEmpty)
        #expect(try await reopened.send(request()).outcome == .applied)
        #expect(try await reopened.send(request(replacementIntentID, epoch: "second-epoch")).outcome == .applied)
        #expect(await peer.posts.count == 2)
    }

    @Test func reconciliationReadsOnlyItsEpochAndKeepsALateResultPending() async throws {
        let directory = try TestFiles.directory()
        let peer = try IntentPeer()
        let sender = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope))
        await #expect(throws: OperationError.outcomeUnknown) { try await sender.send(request()) }
        await sender.reconcilePending(dataEpoch: "other-epoch")
        #expect(await peer.recoveries.isEmpty)
        let checks = Mutex(0)
        await sender.reconcilePending(dataEpoch: "epoch", isCurrent: {
            checks.withLock { count in count += 1; return count == 1 }
        })
        #expect(await sender.unresolvedCommandIDs() == [originalIntentID])
        #expect(await peer.posts.count == 1)
        let restored = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope))
        #expect(await restored.unresolvedCommandIDs() == [originalIntentID])
        await restored.reconcilePending(dataEpoch: "epoch")
        #expect(await restored.unresolvedCommandIDs().isEmpty)
        #expect(await peer.posts.count == 1)
        #expect(await peer.recoveries.map(\.commandId) == [originalIntentID, originalIntentID])
    }

    @Test func corruptedStorageFailsClosedAndIsNotOverwritten() async throws {
        let directory = try TestFiles.directory()
        let peer = try IntentPeer()
        let first = ViewActionSender(transport: peer, intents:
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope))
        await #expect(throws: OperationError.outcomeUnknown) { try await first.send(request()) }
        let file = try #require(try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil).first(where: { $0.pathExtension == "intents" }))
        var corrupt = try Data(contentsOf: file)
        corrupt[corrupt.startIndex] ^= 1
        try corrupt.write(to: file)
        await #expect(throws: ViewActionIntentError.unreadableStore) {
            try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope)
        }
        #expect(try Data(contentsOf: file) == corrupt)
        #expect(await peer.posts.count == 1)
        #expect(await peer.recoveries.isEmpty)
    }
}
