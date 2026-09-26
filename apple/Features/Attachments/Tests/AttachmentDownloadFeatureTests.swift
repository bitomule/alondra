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
