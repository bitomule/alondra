import AlondraAttachmentsFeature
import AlondraContracts
import AlondraDomain
import AlondraNewWorkFeature
import AlondraShared
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct NewRequestFeatureTests {
    @Test func requestsAreAcceptedWhileTheMacIsConnectedAndStopOnDisconnect() async throws {
        let workspace = DomainWorkspace(.init(
            protocolVersion: 2, dataEpoch: "epoch", revision: 1, inbox: [], counts: .init(roots: 0, pending: 0, unreadInformation: 0),
            tasks: [], agents: [], requests: [.init(id: "r", body: "Earlier", closed: false)], interactions: [], pullRequests: [],
            documents: [], documentRevisions: [], documentReviews: [], mergeAuthorizations: [], stackReadiness: []))
        let store = TestStore(initialState: NewRequestFeature.State()) { NewRequestFeature() }
        await store.send(.workspaceUpdated(workspace)) {
            $0.hasSnapshot = true
            $0.canSubmit = true
            $0.sentRequests = [.init(id: "r", body: "Earlier", closed: false)]
        }
        await store.send(.workspaceUpdated(nil)) {
            $0.hasSnapshot = false
            $0.canSubmit = false
            $0.sentRequests = []
        }
    }

    @Test
    func projectChangeDiscardsOldCatalogAndIssueWhileKeepingNewDefaultsAutomatic() async throws {
        let a = try catalog(#"{"project":"A","options":[{"id":"effort","label":"Effort","kind":"choice","values":[{"id":"auto","label":"Auto"},{"id":"hard","label":"Hard"}],"default":"auto"}]}"#)
        let b = try catalog(#"{"project":"B","options":[{"id":"effort","label":"Effort","kind":"choice","values":[{"id":"auto","label":"Auto"}],"default":"auto"}]}"#)
        var initial = NewRequestFeature.State()
        initial.selection.project = "A"
        initial.projects = ["A", "B"]
        let store = TestStore(initialState: initial) { NewRequestFeature() } withDependencies: {
            $0.uuid = .constant(UUID(0))
            $0.newWorkOperations = operations(options: { _ in b })
        }
        await store.send(.optionsLoaded(a, 0, 0)) { $0.selection.apply(a); $0.catalogIsCurrent = true }
        await store.send(.optionSelected("effort", .value("hard"))) {
            $0.selection.set(a.options[0], to: .value("hard"))
        }
        await store.send(.issueSelected("owner/A#7")) { $0.selection.issue = "owner/A#7" }
        await store.send(.projectSelected("B")) {
            $0.selection.project = "B"
            $0.catalogGeneration = 1
            $0.isLoadingOptions = true
            $0.catalogIsCurrent = false
        }
        await store.receive(\.optionsLoaded) {
            $0.selection.apply(b)
            $0.isLoadingOptions = false
            $0.catalogIsCurrent = true
        }
        await store.send(.optionsLoaded(a, 0, 0))
        #expect(store.state.selection.project == "B")
        #expect(store.state.selection.issue == nil)
        #expect(store.state.selection.current(b.options[0]) == .value("auto"))
        #expect(store.state.selection.chosen.isEmpty)
        await store.send(.optionSelected("effort", .value("hard")))
        #expect(store.state.selection.chosen.isEmpty, "A late value from project A must not become an override for B")
        await store.finish()
    }

    @Test
    func submitWaitsForCurrentProjectCatalog() async throws {
        let catalog = try catalog(#"{"project":"A","options":[]}"#)
        var initial = NewRequestFeature.State()
        initial.selection.project = "A"
        initial.isLoadingOptions = true
        initial.canSubmit = true
        initial.text = "Do work"
        let store = TestStore(initialState: initial) { NewRequestFeature() } withDependencies: {
            $0.uuid = .constant(UUID(0))
            $0.newWorkOperations = operations()
        }
        await store.send(.submitTapped)
        #expect(store.state.successfulSubmissionCount == 0)
        await store.send(.optionsLoaded(catalog, 0, 0)) {
            $0.selection.apply(catalog)
            $0.isLoadingOptions = false
            $0.catalogIsCurrent = true
        }
        await store.send(.submitTapped) { $0.isSubmitting = true; $0.commandID = UUID(0).uuidString.lowercased() }
        await store.receive(\.submitFinished) {
            $0.isSubmitting = false
            $0.commandID = nil
            $0.text = ""
            $0.successfulSubmissionCount = 1
        }
        await store.finish()
    }

    @Test
    func issueSelectionInfersProjectBeforeLoadingItsCatalog() async throws {
        let b = try catalog(#"{"project":"B","options":[]}"#)
        var initial = NewRequestFeature.State()
        initial.projects = ["B"]
        let store = TestStore(initialState: initial) { NewRequestFeature() } withDependencies: {
            $0.uuid = .constant(UUID(0))
            $0.newWorkOperations = operations(options: { _ in b })
        }
        await store.send(.issueSelected("owner/B#4")) {
            $0.selection.select(issue: "owner/B#4", inferredProject: "B")
            $0.catalogGeneration = 1
            $0.isLoadingOptions = true
        }
        await store.receive(\.optionsLoaded) {
            $0.selection.apply(b)
            $0.isLoadingOptions = false
            $0.catalogIsCurrent = true
        }
        #expect(store.state.selection.issue == "owner/B#4")
        await store.finish()
    }

    @Test
    func lateProjectListStillInfersCheckoutForSelectedIssue() async throws {
        let b = try catalog(#"{"project":"B","options":[]}"#)
        let store = TestStore(initialState: NewRequestFeature.State()) { NewRequestFeature() } withDependencies: {
            $0.uuid = .constant(UUID(0))
            $0.newWorkOperations = operations(options: { _ in b })
        }
        await store.send(.issueSelected("owner/B#4")) { $0.selection.issue = "owner/B#4" }
        await store.send(.projectsLoaded(["B"], 0)) {
            $0.projects = ["B"]
            $0.selection.select(issue: "owner/B#4", inferredProject: "B")
            $0.catalogGeneration = 1
            $0.isLoadingOptions = true
        }
        await store.receive(\.optionsLoaded) {
            $0.selection.apply(b)
            $0.isLoadingOptions = false
            $0.catalogIsCurrent = true
        }
        #expect(store.state.selection.issue == "owner/B#4")
        await store.finish()
    }

    @Test
    func submitOnceClearsDraftOnlyOnConfirmation() async {
        let attachment = NewRequestAttachment(id: UUID(uuidString: "00000000-0000-0000-0000-000000000001")!, name: "proof.txt", data: Data("proof".utf8))
        var initial = NewRequestFeature.State()
        initial.catalogIsCurrent = true
        let confirmation = AsyncStream<Void>.makeStream()
        let started = AsyncStream<Void>.makeStream()
        let sent = SentBody()
        let store = TestStore(initialState: initial) { NewRequestFeature() } withDependencies: {
            $0.uuid = .constant(UUID(0))
            $0.newWorkOperations = operations(upload: { _ in "stored-1" }, create: { body, _ in
                await sent.record(body)
                started.continuation.yield(())
                for await _ in confirmation.stream { return .confirmed }
                return .cancelled
            })
        }
        await store.send(.availabilityChanged(true)) { $0.canSubmit = true }
        await store.send(.textChanged(" first ")) { $0.text = " first " }
        await store.send(.attachmentAdded(attachment)) { $0.attachments = [attachment] }
        await store.send(.submitTapped) { $0.isSubmitting = true; $0.commandID = UUID(0).uuidString.lowercased() }
        var iterator = started.stream.makeAsyncIterator()
        _ = await iterator.next()
        await store.send(.submitTapped)
        #expect(store.state.text == " first " && store.state.attachments == [attachment])
        #expect(await sent.calls == 1)
        confirmation.continuation.yield(())
        confirmation.continuation.finish()
        await store.receive(\.submitFinished) {
            $0.isSubmitting = false
            $0.commandID = nil
            $0.text = ""
            $0.attachments = []
            $0.successfulSubmissionCount = 1
        }
        await store.finish()
        #expect(await sent.calls == 1)
        started.continuation.finish()
    }

    @Test
    func anUnknownOutcomeKeepsTheDraftAndSendingAgainReusesTheCommandID() async {
        var initial = NewRequestFeature.State()
        initial.catalogIsCurrent = true
        let sent = SentBody()
        let store = TestStore(initialState: initial) { NewRequestFeature() } withDependencies: {
            $0.uuid = .constant(UUID(0))
            $0.newWorkOperations = operations(create: { body, command in await sent.record(body, command); return .uncertain("no acknowledgement") })
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.availabilityChanged(true))
        await store.send(.textChanged("Do work"))
        await store.send(.submitTapped)
        await store.receive(\.submitFinished)
        #expect(store.state.submissionUncertain)
        await store.send(.textChanged("Duplicate"))
        #expect(store.state.text == "Do work", "The draft cannot change while its outcome is unknown")
        await store.send(.submitTapped)
        await store.receive(\.submitFinished)
        let commands = await sent.commands
        #expect(commands.count == 2)
        #expect(commands[0] == commands[1], "Sending again reuses the command ID so the request is created once")
        await store.send(.discardUncertainDraft)
        #expect(store.state.text == "")
        #expect(store.state.commandID == nil)
    }

    @Test
    func failedCatalogStillSendsWithServerDefaults() async throws {
        var initial = NewRequestFeature.State()
        initial.selection.project = "A"
        initial.isLoadingOptions = true
        initial.canSubmit = true
        initial.text = "Do work"
        let sent = SentBody()
        let store = TestStore(initialState: initial) { NewRequestFeature() } withDependencies: {
            $0.uuid = .constant(UUID(0))
            $0.newWorkOperations = operations(create: { body, _ in await sent.record(body); return .confirmed })
        }
        #expect(!store.state.submitEnabled)
        await store.send(.optionsFailed("catalog unavailable", 0, 0)) {
            $0.isLoadingOptions = false
            $0.optionsError = "catalog unavailable"
        }
        #expect(store.state.submitEnabled)
        await store.send(.submitTapped) { $0.isSubmitting = true; $0.commandID = UUID(0).uuidString.lowercased() }
        await store.receive(\.submitFinished) {
            $0.isSubmitting = false
            $0.commandID = nil
            $0.text = ""
            $0.successfulSubmissionCount = 1
        }
        #expect(await sent.body?.project == "A")
        #expect(await sent.body?.options.isEmpty == true)
        await store.finish()
    }

    @Test
    func catalogWithErrorStillSends() async throws {
        let failing = try catalog(#"{"project":"A","options":[],"error":"repo unreadable"}"#)
        var initial = NewRequestFeature.State()
        initial.selection.project = "A"
        initial.isLoadingOptions = true
        initial.canSubmit = true
        initial.text = "Do work"
        let store = TestStore(initialState: initial) { NewRequestFeature() }
        await store.send(.optionsLoaded(failing, 0, 0)) {
            $0.selection.apply(failing)
            $0.isLoadingOptions = false
            $0.optionsError = "repo unreadable"
        }
        #expect(store.state.submitEnabled)
    }

    @Test
    func uncertainDraftCanBeEditedAgainAfterChecking() async {
        var initial = NewRequestFeature.State()
        initial.catalogIsCurrent = true
        let store = TestStore(initialState: initial) { NewRequestFeature() } withDependencies: {
            $0.uuid = .constant(UUID(0))
            $0.newWorkOperations = operations(create: { _, _ in .uncertain("no acknowledgement") })
            $0.uuid = .constant(UUID(0))
        }
        await store.send(.availabilityChanged(true)) { $0.canSubmit = true }
        await store.send(.textChanged("Do work")) { $0.text = "Do work" }
        await store.send(.submitTapped) { $0.isSubmitting = true; $0.commandID = UUID(0).uuidString.lowercased() }
        await store.receive(\.submitFinished) {
            $0.isSubmitting = false
            $0.submissionError = "no acknowledgement"
            $0.submissionUncertain = true
        }
        #expect(store.state.submitEnabled, "Sending again retries with the same command ID")
        await store.send(.editUncertainDraft) {
            $0.commandID = nil
            $0.submissionError = nil
            $0.submissionUncertain = false
        }
        #expect(store.state.submitEnabled)
        await store.send(.textChanged("Do work, again")) { $0.text = "Do work, again" }
        await store.finish()
    }

    private func catalog(_ json: String) throws -> OptionsCatalog {
        try JSONDecoder().decode(OptionsCatalog.self, from: Data(json.utf8))
    }

    @Test
    func sendingRetriesFailedAttachmentAndWaitsForSameTransferReceipt() async throws {
        let id = UUID(uuidString: "00000000-0000-0000-0000-000000000002")!
        let draft = AttachmentDraft(id: id, name: "proof.txt", data: Data("proof".utf8))
        var initial = NewRequestFeature.State()
        initial.canSubmit = true
        initial.catalogIsCurrent = true
        initial.text = "Use the proof"
        initial.attachments = [NewRequestAttachment(id: id, name: draft.name, data: draft.data)]
        initial.attachmentPicker.entries = [.init(draft: draft, status: .failed("Lost receipt"))]
        let receipt = AsyncStream<String>.makeStream()
        let confirmation = AsyncStream<Void>.makeStream()
        let sent = SentBody()
        let store = TestStore(initialState: initial) { NewRequestFeature() } withDependencies: {
            $0.uuid = .constant(UUID(0))
            $0.attachmentUpload = AttachmentUploadClient(ids: { retry in
                #expect(retry.id == id)
                #expect(retry.data == draft.data)
                for await storedID in receipt.stream { return storedID }
                throw CancellationError()
            })
            $0.newWorkOperations = operations(upload: { attachment in
                #expect(attachment.id == id)
                #expect(attachment.stored?.id == "stored-proof", "The final use case must receive the existing receipt")
                return "stored-proof"
            }, create: { body, _ in
                await sent.record(body)
                for await _ in confirmation.stream { return .confirmed }
                return .cancelled
            })
        }
        await store.send(.submitTapped) { $0.pendingSubmission = .awaitingReceipts }
        await store.receive(\.attachmentPicker.prepareForSubmission) { $0.attachmentPicker.entries[0].status = .queued }
        await store.receive(\.attachmentPicker.uploadNext) { $0.attachmentPicker.entries[0].status = .uploading }
        await store.send(.submitTapped)
        #expect(await sent.body == nil)
        receipt.continuation.yield("stored-proof")
        receipt.continuation.finish()
        await store.receive(\.attachmentPicker.uploaded) {
            $0.attachmentPicker.entries[0].status = .uploaded(stored("stored-proof"))
            $0.attachments = [NewRequestAttachment(id: id, name: draft.name, data: draft.data, stored: stored("stored-proof"))]
            $0.pendingSubmission = nil
            $0.isSubmitting = true
            $0.commandID = UUID(0).uuidString.lowercased()
        }
        await store.receive(\.attachmentPicker.uploadNext)
        confirmation.continuation.yield(())
        confirmation.continuation.finish()
        await store.receive(\.submitFinished) {
            $0.isSubmitting = false
            $0.commandID = nil
            $0.text = ""
            $0.attachments = []
            $0.attachmentPicker = AttachmentFeature.State()
            $0.successfulSubmissionCount = 1
        }
        #expect(await sent.body?.attachments.map(\.id) == ["stored-proof"])
        #expect(await sent.calls == 1)
        await store.finish()
    }

    @Test(arguments: [PendingSendCancellation.edit, .disappear, .identity, .remove])
    func lateUploadCannotSubmitAfterIntentIsCancelled(_ cancellation: PendingSendCancellation) async {
        let draft = AttachmentDraft(id: UUID(uuidString: "00000000-0000-0000-0000-000000000003")!, name: "proof.txt", data: Data("proof".utf8))
        var initial = NewRequestFeature.State()
        initial.canSubmit = true
        initial.catalogIsCurrent = true
        initial.text = "Original draft"
        initial.attachmentPicker.entries = [.init(draft: draft, status: .failed("Offline"))]
        initial.attachments = [.init(id: draft.id, name: draft.name, data: draft.data)]
        let receipt = AsyncStream<String>.makeStream()
        let sent = SentBody()
        let store = TestStore(initialState: initial) { NewRequestFeature() } withDependencies: {
            $0.uuid = .constant(UUID(0))
            $0.attachmentUpload = AttachmentUploadClient(ids: { _ in
                for await value in receipt.stream { return value }
                throw CancellationError()
            })
            $0.newWorkOperations = operations(create: { body, _ in
                await sent.record(body)
                return .confirmed
            })
        }
        // Integration of two reducers: assert the externally relevant final command and
        // retained draft, allowing attachment cancellation bookkeeping to evolve.
        store.exhaustivity = .off(showSkippedAssertions: true)
        await store.send(.submitTapped)
        await store.receive(\.attachmentPicker.uploadNext)
        #expect(store.state.pendingSubmission == .awaitingReceipts)
        switch cancellation {
        case .edit: await store.send(.textChanged("Edited draft"))
        case .disappear: await store.send(.disappeared)
        case .identity: await store.send(.identityChanged)
        case .remove: await store.send(.attachmentPicker(.remove(draft.id)))
        }
        receipt.continuation.yield("stored-proof")
        receipt.continuation.finish()
        await store.finish()
        #expect(store.state.pendingSubmission == nil)
        #expect(!store.state.isSubmitting)
        #expect(store.state.successfulSubmissionCount == 0)
        #expect(await sent.calls == 0, "A completed upload must not revive a cancelled Send gesture")
        switch cancellation {
        case .edit: #expect(store.state.text == "Edited draft")
        case .disappear, .remove: #expect(store.state.text == "Original draft")
        case .identity: #expect(store.state.text.isEmpty)
        }
    }

    @Test
    func failedUploadCancelsSendAndManualAttachmentRetryDoesNotReviveIt() async {
        let draft = AttachmentDraft(id: UUID(uuidString: "00000000-0000-0000-0000-000000000004")!, name: "proof.txt", data: Data("proof".utf8))
        var initial = NewRequestFeature.State()
        initial.canSubmit = true
        initial.catalogIsCurrent = true
        initial.text = "Keep this draft"
        initial.attachmentPicker.entries = [.init(draft: draft, status: .failed("Offline"))]
        initial.attachments = [.init(id: draft.id, name: draft.name, data: draft.data)]
        let uploads = UploadAttempts()
        let sent = SentBody()
        let store = TestStore(initialState: initial) { NewRequestFeature() } withDependencies: {
            $0.uuid = .constant(UUID(0))
            $0.attachmentUpload = AttachmentUploadClient(ids: { _ in try await uploads.next() })
            $0.newWorkOperations = operations(create: { body, _ in
                await sent.record(body)
                return .confirmed
            })
        }
        store.exhaustivity = .off(showSkippedAssertions: true)
        await store.send(.submitTapped)
        await store.receive(\.attachmentPicker.uploaded)
        #expect(store.state.pendingSubmission == nil)
        #expect(store.state.submissionError != nil)
        #expect(store.state.text == "Keep this draft")
        #expect(store.state.attachmentPicker.entries.first?.draft == draft)
        await store.send(.attachmentPicker(.retry(draft.id)))
        await store.receive(\.attachmentPicker.uploaded)
        await store.finish()
        #expect(store.state.attachmentPicker.storedIDs == ["stored-proof"])
        #expect(store.state.text == "Keep this draft")
        #expect(!store.state.isSubmitting)
        #expect(await sent.calls == 0)
    }

    private func operations(
        options: @escaping @Sendable (String?) async throws -> OptionsCatalog = { _ in throw SubmissionFailure.refused },
        upload: @escaping @Sendable (NewRequestAttachment) async throws -> String = { _ in "stored" },
        create: @escaping @Sendable (NewRequestBody, String) async -> CreateRequestResult = { _, _ in .confirmed }
    ) -> NewWorkOperations {
        NewWorkOperations(
            projects: { [] }, options: options,
            issues: { _ in try JSONDecoder().decode(IssueList.self, from: Data(#"{"issues":[]}"#.utf8)) },
            captureSubmissionScope: { NewWorkSubmissionScope(upload: { stored(try await upload($0)) }, isCurrent: { true }, create: create) }
        )
    }
}

private enum SubmissionFailure: Error, LocalizedError {
    case refused
    var errorDescription: String? { "refused" }
}

private actor SentBody {
    private(set) var body: NewRequestBody?
    private(set) var calls = 0
    private(set) var commands: [String] = []
    func record(_ body: NewRequestBody, _ command: String = "") { self.body = body; calls += 1; commands.append(command) }
}

func stored(_ id: String) -> DomainV2.AttachmentRef {
    .init(id: id, name: "proof.txt", contentDigest: String(repeating: "a", count: 64), contentReference: "files/" + id, bytes: 5)
}

enum PendingSendCancellation: Sendable { case edit, disappear, identity, remove }

private actor UploadAttempts {
    private var attempted = false
    func next() throws -> String {
        if !attempted {
            attempted = true
            throw AttachmentFailure(message: "Still offline")
        }
        return "stored-proof"
    }
}
