import AlondraAttachmentsFeature
import AlondraNewWorkFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct AttachmentLifecycleTests {
    @Test(arguments: [true, false])
    func exceedingBudgetDoesNotRetainRejectedBytesOrEnablePartialSend(countLimit: Bool) async {
        let first = AttachmentDraft(name: "first", data: Data([1, 2, 3]))
        let rejected = AttachmentDraft(name: "second", data: Data([4, 5, 6]))
        let batch = UUID()
        var initial = AttachmentFeature.State()
        initial.entries = [.init(draft: first, status: .uploaded("stored-first"))]
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
    func replacingIdentityCancelsSuspendedImportBeforeNextFile() async {
        let gate = SuspendedAttachmentImport()
        let batch = UUID()
        let store = TestStore(initialState: NewRequestFeature.State()) { NewRequestFeature() } withDependencies: {
            $0.uuid = .constant(batch)
            $0.attachmentImport = AttachmentImportClient(file: { try await gate.read($0) }, photo: { _ in
                throw AttachmentFailure(message: "Unexpected photo import")
            })
            $0.attachmentUpload = AttachmentUploadClient(upload: { _ in
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
