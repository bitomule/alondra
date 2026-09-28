import AlondraAttachmentsFeature
import AlondraContracts
import AlondraNewWorkFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct AttachmentLifecycleTests {
    @Test
    func cameraAttachmentWaitsForServerReceiptEvenAfterAllBytesUpload() async {
        let draft = AttachmentDraft(name: "Photo.jpg", data: Data([1, 2, 3]))
        let batch = UUID()
        let store = TestStore(initialState: AttachmentFeature.State()) { AttachmentFeature() } withDependencies: {
            $0.uuid = .constant(batch)
            $0.attachmentImport = AttachmentImportClient(file: { _ in throw CancellationError() },
                photo: { _ in throw CancellationError() }, camera: { bytes in
                    #expect(bytes == draft.data)
                    return draft
                })
            $0.attachmentUpload = AttachmentUploadClient(idsWithProgress: { uploaded, progress in
                #expect(uploaded == draft)
                progress(1)
                return "server-receipt"
            })
        }
        await store.send(.showCamera(true)) { $0.cameraPresented = true }
        await store.send(.cameraCaptured(draft.data)) {
            $0.cameraPresented = false
            $0.importID = batch
            $0.isImporting = true
        }
        await store.receive(\.imported) { $0.entries = [.init(draft: draft)] }
        await store.receive(\.uploadNext) { $0.entries[0].status = .uploading }
        await store.receive(\.importFinished) { $0.importID = nil; $0.isImporting = false }
        await store.receive(\.uploadNext)
        await store.receive(\.uploadProgress) { $0.entries[0].progress = 1 }
        #expect(!store.state.allReady)
        #expect(store.state.storedIDs.isEmpty)
        await store.receive(\.uploaded) { $0.entries[0].status = .uploaded(stored("server-receipt")) }
        await store.receive(\.uploadNext)
        #expect(store.state.allReady)
        #expect(store.state.storedIDs == ["server-receipt"])
        await store.finish()
    }

    @Test(arguments: [true, false])
    func exceedingBudgetDoesNotRetainRejectedBytesOrEnablePartialSend(countLimit: Bool) async {
        let first = AttachmentDraft(name: "first", data: Data([1, 2, 3]))
        let rejected = AttachmentDraft(name: "second", data: Data([4, 5, 6]))
        let batch = UUID()
        var initial = AttachmentFeature.State()
        initial.entries = [.init(draft: first, status: .uploaded(stored("stored-first")))]
        initial.importID = batch
        initial.isImporting = true
        let store = TestStore(initialState: initial) { AttachmentFeature() } withDependencies: {
            $0.attachmentBudget = AttachmentBudget(maximumBytes: countLimit ? 100 : 5, maximumCount: countLimit ? 1 : 10)
        }
        await store.send(.imported(rejected, batch)) {
            $0.importID = nil
            $0.isImporting = false
            $0.importError = "Attachment limit exceeded. Remove files or continue with only the selected attachments."
        }
        #expect(store.state.entries.map(\.draft) == [first])
        #expect(!store.state.allReady)
        // An already queued callback cannot silently erase the explicit partial-import warning.
        await store.send(.importFinished(batch))
        #expect(!store.state.allReady)
        await store.send(.dismissError) { $0.importError = nil }
        #expect(store.state.allReady)
        #expect(store.state.storedIDs == ["stored-first"])
        await store.finish()
    }

    @Test
    func failedUploadKeepsOriginalBytesAndOnlyExplicitRetryStartsAnotherTransfer() async {
        let draft = AttachmentDraft(name: "notes.txt", data: Data("notes".utf8))
        let uploads = RetriedAttachmentUpload()
        var initial = AttachmentFeature.State()
        initial.entries = [.init(draft: draft)]
        let store = TestStore(initialState: initial) { AttachmentFeature() } withDependencies: {
            $0.attachmentUpload = AttachmentUploadClient(ids: { try await uploads.send($0) })
        }
        await store.send(.uploadNext) { $0.entries[0].status = .uploading }
        await store.receive(\.uploaded) { $0.entries[0].status = .failed("Lost upload receipt") }
        await store.receive(\.uploadNext)
        #expect(store.state.entries.map(\.draft) == [draft])
        #expect(!store.state.allReady)
        #expect(store.state.storedIDs.isEmpty)
        #expect(await uploads.drafts == [draft], "A failed transfer must not automatically retry")
        await store.send(.retry(draft.id)) { $0.entries[0].status = .queued }
        await store.receive(\.uploadNext) { $0.entries[0].status = .uploading }
        await store.receive(\.uploaded) { $0.entries[0].status = .uploaded(stored("stored-notes")) }
        await store.receive(\.uploadNext)
        await store.send(.retry(draft.id))
        #expect(store.state.entries.map(\.draft) == [draft])
        #expect(store.state.allReady)
        #expect(store.state.storedIDs == ["stored-notes"])
        #expect(await uploads.drafts == [draft, draft], "Retry keeps both bytes and transfer identity")
        await store.finish()
    }

    @Test
    func replacingIdentityCancelsSuspendedImportBeforeNextFile() async {
        let gate = SuspendedAttachmentImport()
        let batch = UUID()
        let store = TestStore(initialState: NewRequestFeature.State()) { NewRequestFeature() } withDependencies: {
            $0.uuid = .constant(batch)
            $0.attachmentImport = AttachmentImportClient(file: { try await gate.read($0) }, photo: { _ in
                throw AttachmentFailure(message: "Unexpected photo import")
            })
            $0.attachmentUpload = AttachmentUploadClient(ids: { _ in
                Issue.record("An obsolete import must not upload")
                return "unexpected"
            })
        }
        await store.send(.attachmentPicker(.filesSelected([
            URL(fileURLWithPath: "/first"), URL(fileURLWithPath: "/second")
        ]))) {
            $0.attachmentPicker.importID = batch
            $0.attachmentPicker.isImporting = true
        }
        await gate.waitUntilReading()
        await store.send(.identityChanged) {
            $0 = NewRequestFeature.State()
            $0.presentationEpoch = 1
        }
        await gate.release()
        await store.finish()
        #expect(await gate.paths == ["/first"])
        #expect(store.state.attachmentPicker.entries.isEmpty)
        #expect(!store.state.attachmentPicker.isImporting)
        #expect(store.state.attachments.isEmpty)
    }
}

private actor SuspendedAttachmentImport {
    private(set) var paths: [String] = []
    private var releaseContinuation: CheckedContinuation<Void, Never>?
    private var startedContinuation: CheckedContinuation<Void, Never>?
    func read(_ url: URL) async throws -> AttachmentDraft {
        paths.append(url.path)
        if paths.count > 1 { return AttachmentDraft(name: url.lastPathComponent, data: Data([2])) }
        await withCheckedContinuation { continuation in
            releaseContinuation = continuation
            startedContinuation?.resume()
            startedContinuation = nil
        }
        return AttachmentDraft(name: url.lastPathComponent, data: Data([1]))
    }
    func waitUntilReading() async {
        if releaseContinuation != nil { return }
        await withCheckedContinuation { startedContinuation = $0 }
    }
    func release() {
        releaseContinuation?.resume()
        releaseContinuation = nil
    }
}

private actor RetriedAttachmentUpload {
    private(set) var drafts: [AttachmentDraft] = []
    private struct LostReceipt: LocalizedError {
        var errorDescription: String? { "Lost upload receipt" }
    }
    func send(_ draft: AttachmentDraft) throws -> String {
        drafts.append(draft)
        if drafts.count == 1 { throw LostReceipt() }
        return "stored-notes"
    }
}
