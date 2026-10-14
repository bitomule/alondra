@testable import AlondraFeed
import AlondraContract
import AlondraTransport
import AlondraTestSupport
import Foundation
import SQLite3
import Testing

/// Every iOS container is reached through /var, a link to /private/var. A store that opens its
/// SQLite file with NOFOLLOW on the unresolved path works on the Mac and never opens on an iPhone,
/// so every store the app keeps on disk is opened here the way the device reaches it.
@Suite(.temporaryFiles) struct LinkedContainerStoresTests {
    private struct Container {
        let real: URL
        let linked: URL
    }

    private func container() throws -> Container {
        let root = try TestFiles.directory()
        let real = root.appending(path: "private", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: real, withIntermediateDirectories: true)
        let linked = root.appending(path: "var")
        try FileManager.default.createSymbolicLink(at: linked, withDestinationURL: real)
        return Container(real: real, linked: linked)
    }

    @Test func everyAppStoreOpensInsideALinkedContainer() async throws {
        let container = try container()
        let scope = RelayOutboxScope(relay: URL(string: "https://relay.example")!, macID: "mac", deviceID: "phone", credentialRevision: "credential")
        let journal = try EnvelopeJournal(scope: scope, directory: container.linked.appending(path: "operations"))
        _ = try await journal.entries()
        try await journal.retire(ActionJournalAcknowledgment(dataEpoch: "epoch", generation: 1, ranges: []))

        let intents = try await ViewActionIntentStore(directory: container.linked.appending(path: "CommandJournal/action-intents"),
            keyMaterial: Data(repeating: 7, count: 32),
            scope: ActionIntentScope(service: "paired-mac", device: "phone", credentialRevision: "credential"))
        _ = try await intents.acknowledgmentRanges()

        let databases = try FileManager.default.subpathsOfDirectory(atPath: container.real.path).filter { $0.hasSuffix(".sqlite") }
        #expect(databases.count == 2)
    }

    @Test func aDatabaseFileThatIsItselfALinkIsStillRefused() throws {
        let container = try container()
        let target = container.real.appending(path: "target.sqlite")
        #expect(FileManager.default.createFile(atPath: target.path, contents: nil))
        try FileManager.default.createSymbolicLink(at: container.real.appending(path: "store.sqlite"), withDestinationURL: target)

        #expect(try SQLiteFile.open("store.sqlite", in: container.linked, flags: SQLITE_OPEN_READWRITE) == nil)
        let database = try #require(try SQLiteFile.open("target.sqlite", in: container.linked, flags: SQLITE_OPEN_READWRITE))
        sqlite3_close(database)
    }
}
