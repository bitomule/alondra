import AlondraAttachmentsFeature
import AlondraDomain
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct AttachmentDownloadFeatureTests {
    @Test
    func obsoleteDownloadIsReleasedWithoutReplacingCurrentDocument() async {
        let released = ReleasedFiles()
        let old = DownloadedAttachment(url: URL(fileURLWithPath: "/tmp/old"), name: "old")
        let current = DownloadedAttachment(url: URL(fileURLWithPath: "/tmp/current"), name: "current")
        var state = AttachmentDownloadFeature.State()
        state.generation = 2
        state.file = current
        let store = TestStore(initialState: state) { AttachmentDownloadFeature() } withDependencies: {
            $0.attachmentDownload = AttachmentDownloadClient(download: { _, _, _ in current }, release: { await released.append($0) })
        }
        await store.send(.finished(.success(old), 1))
        await store.finish()
        #expect(await released.files == [old])
        #expect(store.state.file == current)
    }

    @Test
    func openingDownloadedFileKeepsLeaseUntilSystemAcceptsIt() async {
        let released = ReleasedFiles()
        let gate = AsyncStream<Void>.makeStream()
        let file = DownloadedAttachment(url: URL(fileURLWithPath: "/tmp/open"), name: "open")
        var state = AttachmentDownloadFeature.State()
        state.isLoading = true
        state.opensWhenReady = true
        let store = TestStore(initialState: state) { AttachmentDownloadFeature() } withDependencies: {
            $0.attachmentDownload = AttachmentDownloadClient(download: { _, _, _ in file }, release: { await released.append($0) })
            $0.openURL = .init { url in
                #expect(url == file.url)
                for await _ in gate.stream { break }
                return true
            }
        }
        await store.send(.finished(.success(file), 0)) {
            $0.isLoading = false; $0.file = file; $0.opensWhenReady = false; $0.isOpening = true
        }
        await store.send(.disappeared) { $0.isVisible = false; $0.generation = 1 }
        #expect(await released.files.isEmpty)
        gate.continuation.yield(())
        gate.continuation.finish()
        await store.receive(\.opened) { $0.isOpening = false; $0.file = nil }
        await store.finish()
        #expect(await released.files == [file])
    }

    @Test
    func disappearingDuringExportRetainsFileUntilSystemCompletion() async {
        let released = ReleasedFiles()
        let file = DownloadedAttachment(url: URL(fileURLWithPath: "/tmp/export"), name: "export")
        var state = AttachmentDownloadFeature.State()
        state.file = file
        let store = TestStore(initialState: state) { AttachmentDownloadFeature() } withDependencies: {
            $0.attachmentDownload = AttachmentDownloadClient(download: { _, _, _ in file }, release: { await released.append($0) })
        }
        await store.send(.export) { $0.exporting = true }
        await store.send(.disappeared) { $0.isVisible = false; $0.generation = 1 }
        #expect(await released.files.isEmpty)
        await store.send(.exportFinished(nil)) { $0.exporting = false; $0.file = nil }
        await store.finish()
        #expect(await released.files == [file])
    }
}

private actor ReleasedFiles {
    var files: [DownloadedAttachment] = []
    func append(_ file: DownloadedAttachment) { files.append(file) }
}
