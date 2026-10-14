@testable import AlondraFeed
import AlondraContract
import AlondraTransport
import AlondraTestSupport
import Foundation
import Testing

private let answerID = "00000000-0000-4000-8000-0000000000a1"

private actor SilentMac: ViewTransport {
    private(set) var posts: [ActionRequest] = []
    private(set) var recoveries: [ActionRequest] = []
    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch { throw FeedError.unknownCommand }
    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed { throw FeedError.unknownCommand }
    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        posts.append(request)
        throw OperationError.outcomeUnknown
    }
    func recoverAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse? {
        recoveries.append(request)
        return nil
    }
}

@Suite(.temporaryFiles) struct UnreadableActionStoreTests {
    private let material = Data(repeating: 7, count: 32)
    private let scope = ActionIntentScope(service: "paired-mac", device: "phone", credentialRevision: "credential")
    private let answer = ActionRequest(commandId: answerID, dataEpoch: "epoch", client: .ios, lang: "en",
        actionId: "ans:question@1", optionId: "yes", input: ActionInput(text: "Merge it"), resolvesCard: true)

    private func files(_ directory: URL) throws -> [String] {
        try FileManager.default.contentsOfDirectory(atPath: directory.path).sorted()
    }

    /// Every iOS container is reached through /var, a link to /private/var: SQLite's NOFOLLOW
    /// refused the whole path, so builds 45 and 46 never opened their store on an iPhone.
    @Test func aStoreReachedThroughALinkedFolderOpensAndKeepsItsAnswers() async throws {
        let root = try TestFiles.directory()
        let real = root.appending(path: "private", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: real, withIntermediateDirectories: true)
        let linked = root.appending(path: "var")
        try FileManager.default.createSymbolicLink(at: linked, withDestinationURL: real)
        let mac = SilentMac()

        let store = try await ViewActionIntentStore(directory: linked.appending(path: "CommandJournal/action-intents"), keyMaterial: material,
                                                    scope: scope, allowLegacyDispatch: true)
        await #expect(throws: OperationError.outcomeUnknown) { try await ViewActionSender(transport: mac, intents: store).send(answer) }

        let reopened = try await ViewActionIntentStore(directory: real.appending(path: "CommandJournal/action-intents"), keyMaterial: material, scope: scope)
        #expect(reopened.restored.map(\.request.commandId) == [answerID])
        #expect(try files(real.appending(path: "CommandJournal/action-intents")).contains { $0.hasSuffix(".sqlite") })
    }

    /// The only change to what 0.3.0 (45) wrote is the metadata's `floor`, which it never had.
    @Test func metadataWrittenByBuild45ReadsWithTheFirstSequenceAsItsFloor() throws {
        let build45 = #"{"activeEpoch":"epoch","generation":3,"next":9,"retired":[{"first":2,"last":4}]}"#
        let metadata = try JSONDecoder().decode(ActionIntentDatabase.Metadata.self, from: Data(build45.utf8))
        #expect(metadata.floor == 1 && metadata.next == 9 && metadata.acknowledged.isEmpty)
    }

    @Test func aStoreThatCannotBeReadIsSetAsideWithTheAnswersItStillShows() async throws {
        let directory = try TestFiles.directory()
        let mac = SilentMac()
        let original = try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope, allowLegacyDispatch: true)
        await #expect(throws: OperationError.outcomeUnknown) { try await ViewActionSender(transport: mac, intents: original).send(answer) }
        let marker = try #require(try files(directory).first { $0.hasSuffix(".sqlite-initialized") })
        try Data(repeating: 1, count: 32).write(to: directory.appending(path: marker))

        let opened = await ViewActionIntentStore.openSettingAsideUnreadable(directory: directory, keyMaterial: material, scope: scope,
                                                                            now: { Date(timeIntervalSince1970: 1_791_000_000) })

        let store = try #require(opened.store)
        #expect(opened.setAside.current == SetAsideAnswers(cards: ["question"], otherActions: 0, complete: true))
        let sender = ViewActionSender(transport: mac, intents: store, setAside: opened.setAside)
        #expect(await sender.unresolvedCommandIDs().isEmpty, "a set-aside answer is never sent again")
        #expect(sender.undelivered.current.isEmpty, "its card comes back to the queue")
        #expect(await mac.posts.count == 1)
        #expect(await mac.recoveries.isEmpty)
        let folders = try files(ActionIntentQuarantine.root(directory))
        #expect(folders.count == 1)
        #expect(try files(ActionIntentQuarantine.root(directory).appending(path: folders[0])).contains { $0.hasSuffix(".sqlite") })
    }

    /// The Mac may still hold the old store's sequences in this generation, and one reused under
    /// another identity is never accepted: the new store starts past all of them.
    @Test func theStoreThatReplacesOneSetAsideNeverReusesItsSequences() async throws {
        let directory = try TestFiles.directory()
        _ = try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope)
        let file = try #require(try files(directory).first { $0.hasSuffix(".sqlite") })
        try Data(repeating: 0, count: 4096).write(to: directory.appending(path: file))
        let now = Date(timeIntervalSince1970: 1_791_000_000)

        let opened = await ViewActionIntentStore.openSettingAsideUnreadable(directory: directory, keyMaterial: material, scope: scope, now: { now })
        let store = try #require(opened.store)
        let prepared = try await store.prepareNew(answer, generation: 2)

        let operation = try #require(ActionOperationID(rawValue: prepared.request.commandId))
        #expect(operation.generation == 2)
        #expect(operation.sequence == ViewActionIntentStore.sequenceFloor(at: now))
        #expect(operation.sequence > 1_700_000_000_000)
    }

    @Test func aStoreThatShowsNothingSaysSomethingMayBeMissingUntilDismissed() async throws {
        let directory = try TestFiles.directory()
        _ = try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope)
        let file = try #require(try files(directory).first { $0.hasSuffix(".sqlite") })
        var corrupt = try Data(contentsOf: directory.appending(path: file))
        corrupt.replaceSubrange(0..<16, with: Data(repeating: 0, count: 16))
        try corrupt.write(to: directory.appending(path: file))

        let first = await ViewActionIntentStore.openSettingAsideUnreadable(directory: directory, keyMaterial: material, scope: scope)
        #expect(first.store != nil)
        #expect(first.setAside.current == SetAsideAnswers(cards: [], otherActions: 0, complete: false))

        let relaunched = await ViewActionIntentStore.openSettingAsideUnreadable(directory: directory, keyMaterial: material, scope: scope)
        #expect(relaunched.setAside.current == first.setAside.current, "a relaunch before the person saw it still shows it")
        relaunched.setAside.dismiss()
        #expect(relaunched.setAside.current == nil)

        let afterDismissing = await ViewActionIntentStore.openSettingAsideUnreadable(directory: directory, keyMaterial: material, scope: scope)
        #expect(afterDismissing.setAside.current == nil)
        #expect(afterDismissing.store != nil)
    }

    @Test func onlyTheNewestStoresSetAsideAreKept() async throws {
        let directory = try TestFiles.directory()
        for second in 0..<5 {
            _ = try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope)
            let file = try #require(try files(directory).first { $0.hasSuffix(".sqlite") })
            try Data(repeating: 0, count: 4096).write(to: directory.appending(path: file))
            _ = await ViewActionIntentStore.openSettingAsideUnreadable(directory: directory, keyMaterial: material, scope: scope,
                                                                       now: { Date(timeIntervalSince1970: 1_791_000_000 + Double(second)) })
        }
        #expect(try files(ActionIntentQuarantine.root(directory)).count == ActionIntentQuarantine.kept)
    }

    @Test func aStoreSetAsideWithNothingInItNeedsNoNotice() async throws {
        let directory = try TestFiles.directory()
        _ = try await ViewActionIntentStore(directory: directory, keyMaterial: material, scope: scope)
        let marker = try #require(try files(directory).first { $0.hasSuffix(".sqlite-initialized") })
        try Data(repeating: 1, count: 32).write(to: directory.appending(path: marker))

        let opened = await ViewActionIntentStore.openSettingAsideUnreadable(directory: directory, keyMaterial: material, scope: scope)
        #expect(opened.store != nil)
        #expect(opened.setAside.current == nil)
    }
}
