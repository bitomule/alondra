import AlondraContract
import AlondraTestSupport
@testable import AlondraDesign
import Foundation
import Synchronization
import Testing

@MainActor @Suite(.temporaryFiles)
struct CardAttachmentFilesTests {
    private static let image = Attachment(id: "shot", name: "shot.PNG", kind: .image, size: 3,
                                          fetch: "/interactions/i-1/attachments/shot/content", digest: String(repeating: "a", count: 64))
    private static let video = Attachment(id: "clip", name: "clip.mp4", kind: .video, size: 3,
                                          fetch: "/interactions/i-1/attachments/clip/content", digest: String(repeating: "b", count: 64))

    @Test func aDownloadedAttachmentIsKeptByItsDigestAndNeverDownloadedAgain() async throws {
        let directory = try TestFiles.directory()
        let calls = Mutex(0)
        let files = CardAttachmentFiles(directory: directory, download: { _, file, progress in
            calls.withLock { $0 += 1 }
            try Data("png".utf8).write(to: file)
            progress(3, 3)
        }, isGone: { _ in false })

        #expect(files.phase(Self.image) == .idle)
        files.load(Self.image)
        files.load(Self.image)
        #expect(files.phase(Self.image) == .loading(fraction: nil))
        await files.settled(Self.image)

        let stored = directory.appendingPathComponent(String(repeating: "a", count: 64) + ".png")
        #expect(files.phase(Self.image) == .ready(stored))
        #expect(try Data(contentsOf: stored) == Data("png".utf8))
        let reopened = CardAttachmentFiles(directory: directory, download: { _, _, _ in Issue.record("downloaded again") },
                                           isGone: { _ in false })
        #expect(reopened.phase(Self.image) == .ready(stored))
        reopened.load(Self.image)
        #expect(calls.withLock { $0 } == 1)
        #expect(try FileManager.default.contentsOfDirectory(atPath: directory.path) == [stored.lastPathComponent])
    }

    @Test func aFailedDownloadSaysWhetherTryingAgainCanHelpAndLeavesNoPartialFile() async throws {
        struct Gone: Error {}
        struct Offline: Error {}
        let directory = try TestFiles.directory()
        let outcome = Mutex<Error?>(Gone())
        let files = CardAttachmentFiles(directory: directory, download: { _, file, _ in
            try Data("half".utf8).write(to: file)
            if let error = outcome.withLock({ $0 }) { throw error }
            try Data("mp4".utf8).write(to: file)
        }, isGone: { $0 is Gone })

        files.load(Self.video)
        await files.settled(Self.video)
        #expect(files.phase(Self.video) == .failed(.gone))

        outcome.withLock { $0 = Offline() }
        files.load(Self.video)
        await files.settled(Self.video)
        #expect(files.phase(Self.video) == .failed(.unreachable))
        #expect(try FileManager.default.contentsOfDirectory(atPath: directory.path).isEmpty)

        outcome.withLock { $0 = nil }
        files.load(Self.video)
        await files.settled(Self.video)
        let stored = directory.appendingPathComponent(String(repeating: "b", count: 64) + ".mp4")
        #expect(files.phase(Self.video) == .ready(stored))
        #expect(try Data(contentsOf: stored) == Data("mp4".utf8))
    }

    @Test func aStoppedDownloadGoesBackToWaitingForATap() async throws {
        let directory = try TestFiles.directory()
        let files = CardAttachmentFiles(directory: directory, download: { _, _, _ in
            try await Task.sleep(for: .seconds(3600))
        }, isGone: { _ in false })

        files.load(Self.video)
        files.cancel(Self.video)

        #expect(files.phase(Self.video) == .idle)
        #expect(try FileManager.default.contentsOfDirectory(atPath: directory.path).isEmpty)
    }

    @Test func theFolderDropsTheOldestFilesToStayUnderItsLimit() throws {
        let directory = try TestFiles.directory()
        for (index, name) in ["old", "middle", "new"].enumerated() {
            let file = directory.appendingPathComponent(name)
            try Data(count: 100).write(to: file)
            try FileManager.default.setAttributes([.modificationDate: Date(timeIntervalSince1970: Double(index) * 1000)],
                                                  ofItemAtPath: file.path)
        }
        let kept = directory.appendingPathComponent("old")

        CardAttachmentFiles.trim(directory, to: 200, keeping: kept)

        #expect(Set(try FileManager.default.contentsOfDirectory(atPath: directory.path)) == ["old", "new"])
    }
}
