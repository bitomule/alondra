import AlondraAttachmentsFeature
import AlondraNewWorkFeature
import AlondraShared
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct NewRequestFeatureTests {
    @Test
    func projectChangeDiscardsOldCatalogAndIssueWhileKeepingNewDefaultsAutomatic() async throws {
        let a = try catalog(#"{"project":"A","options":[{"id":"effort","label":"Effort","kind":"choice","values":[{"id":"auto","label":"Auto"},{"id":"hard","label":"Hard"}],"default":"auto"}]}"#)
        let b = try catalog(#"{"project":"B","options":[{"id":"effort","label":"Effort","kind":"choice","values":[{"id":"auto","label":"Auto"}],"default":"auto"}]}"#)
        var initial = NewRequestFeature.State()
        initial.selection.project = "A"
        initial.projects = ["A", "B"]
        let store = TestStore(initialState: initial) { NewRequestFeature() } withDependencies: {
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
            $0.newWorkOperations = operations()
        }
        await store.send(.submitTapped)
        #expect(store.state.successfulSubmissionCount == 0)
        await store.send(.optionsLoaded(catalog, 0, 0)) {
            $0.selection.apply(catalog)
            $0.isLoadingOptions = false
            $0.catalogIsCurrent = true
        }
        await store.send(.submitTapped) { $0.isSubmitting = true }
        await store.receive(\.submitFinished) {
            $0.isSubmitting = false
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
        let store = TestStore(initialState: initial) { NewRequestFeature() } withDependencies: {
            $0.newWorkOperations = operations(upload: { _ in "stored-1" })
        }
        await store.send(.availabilityChanged(true)) { $0.canSubmit = true }
        await store.send(.textChanged(" first ")) { $0.text = " first " }
        await store.send(.attachmentAdded(attachment)) { $0.attachments = [attachment] }
        await store.send(.submitTapped) { $0.isSubmitting = true }
        await store.send(.submitTapped)
        await store.receive(\.submitFinished) {
            $0.isSubmitting = false
            $0.text = ""
            $0.attachments = []
            $0.successfulSubmissionCount = 1
        }
        await store.finish()
    }

    @Test
    func failedPostIsUncertainAndCannotBeRetriedFromSameDraft() async {
        var initial = NewRequestFeature.State()
        initial.catalogIsCurrent = true
        let store = TestStore(initialState: initial) { NewRequestFeature() } withDependencies: {
            $0.newWorkOperations = operations(create: { _ in .uncertain("no acknowledgement") })
        }
        await store.send(.availabilityChanged(true)) { $0.canSubmit = true }
        await store.send(.textChanged("Do work")) { $0.text = "Do work" }
        await store.send(.submitTapped) { $0.isSubmitting = true }
        await store.receive(\.submitFinished) {
            $0.isSubmitting = false
            $0.submissionError = "no acknowledgement"
            $0.submissionUncertain = true
        }
        await store.send(.submitTapped)
        await store.send(.textChanged("Duplicate"))
        #expect(store.state.text == "Do work")
        await store.send(.discardUncertainDraft) {
            $0.text = ""
            $0.submissionError = nil
            $0.submissionUncertain = false
        }
        await store.finish()
    }

    @Test
    func failedAttachmentUploadNeverSendsPartialRequest() async {
        let attachment = NewRequestAttachment(id: UUID(), name: "broken", data: Data())
        let useCase = CreateRequestUseCase(operations: operations(
            upload: { _ in throw SubmissionFailure.refused },
            create: { _ in Issue.record("create must not be called"); return .confirmed }
        ))
        let body = NewRequestBody(text: "Do work", project: nil, options: [:], issue: nil)
        #expect(await useCase.run(body, [attachment]) == .failed("refused"))
    }

    @Test
    func uploadsAreIncludedInOrderWithProjectIssueAndAutomaticEffort() async {
        let recorder = SentBody()
        let attachments = [
            NewRequestAttachment(id: UUID(), name: "one", data: Data([1])),
            NewRequestAttachment(id: UUID(), name: "two", data: Data([2])),
        ]
        let useCase = CreateRequestUseCase(operations: operations(
            upload: { "stored-\($0.name)" },
            create: { await recorder.record($0); return .confirmed }
        ))
        let body = NewRequestBody(text: "Fix it", project: "Lumo", options: [:], issue: "owner/Lumo#12")
        #expect(await useCase.run(body, attachments) == .confirmed)
        let sent = await recorder.body
        #expect(sent?.attachments == ["stored-one", "stored-two"])
        #expect(sent?.project == "Lumo")
        #expect(sent?.issue == "owner/Lumo#12")
        #expect(sent?.options.isEmpty == true, "No effort override means the server's automatic default remains authoritative")
    }

    @Test
    func identityRotationDuringUploadDoesNotDispatchWithReplacementSigner() async {
        let gate = SubmissionGate()
        let scope = NewWorkSubmissionScope(
            upload: { _ in await gate.rotateToB(); return "stored-A" },
            isCurrent: { await gate.isA },
            create: { _ in await gate.recordDispatch(signer: "A"); return .confirmed }
        )
        let operations = NewWorkOperations(
            projects: { [] },
            options: { _ in throw SubmissionFailure.refused },
            issues: { _ in throw SubmissionFailure.refused },
            captureSubmissionScope: { scope }
        )
        let useCase = CreateRequestUseCase(operations: operations)
        let attachment = NewRequestAttachment(id: UUID(), name: "proof", data: Data([1]))
        let body = NewRequestBody(text: "A request", project: nil, options: [:], issue: nil)
        #expect(await useCase.run(body, [attachment]) == .cancelled)
        #expect(await gate.dispatchedSigner == nil)
    }

    @Test
    func identityRotationBetweenPreflightAndCreateIsRejectedAtScopeBoundary() async {
        let gate = SubmissionGate()
        let scope = NewWorkSubmissionScope(
            upload: { _ in "unused" },
            isCurrent: { await gate.checkThenRotateOnFirstCall() },
            create: { _ in await gate.recordDispatch(signer: "A"); return .confirmed }
        )
        let operations = NewWorkOperations(
            projects: { [] },
            options: { _ in throw SubmissionFailure.refused },
            issues: { _ in throw SubmissionFailure.refused },
            captureSubmissionScope: { scope }
        )
        let body = NewRequestBody(text: "A request", project: nil, options: [:], issue: nil)
        #expect(await CreateRequestUseCase(operations: operations).run(body, []) == .cancelled)
        #expect(await gate.dispatchedSigner == nil)
    }

    private func catalog(_ json: String) throws -> OptionsCatalog {
        try JSONDecoder().decode(OptionsCatalog.self, from: Data(json.utf8))
    }

    @Test
    func failedAttachmentBlocksFinalRequestUntilSameTransferIsRecovered() async throws {
        let id = UUID()
        let draft = AttachmentDraft(id: id, name: "proof.txt", data: Data("proof".utf8))
        var initial = NewRequestFeature.State()
        initial.canSubmit = true
        initial.catalogIsCurrent = true
        initial.text = "Use the proof"
        initial.attachments = [NewRequestAttachment(id: id, name: draft.name, data: draft.data)]
        initial.attachmentPicker.entries = [.init(draft: draft, status: .failed("Lost receipt"))]
        let sent = SentBody()
        let store = TestStore(initialState: initial) { NewRequestFeature() } withDependencies: {
            $0.attachmentUpload = AttachmentUploadClient(upload: { retry in
                #expect(retry.id == id)
                #expect(retry.data == draft.data)
                return "stored-proof"
            })
            $0.newWorkOperations = operations(upload: { attachment in
                #expect(attachment.id == id)
                return "stored-proof"
            }, create: { body in
                await sent.record(body)
                return .confirmed
            })
        }
        await store.send(.submitTapped)
        #expect(await sent.body == nil)
        await store.send(.attachmentPicker(.retry(id))) { $0.attachmentPicker.entries[0].status = .queued }
        await store.receive(\.attachmentPicker.uploadNext) { $0.attachmentPicker.entries[0].status = .uploading }
        await store.receive(\.attachmentPicker.uploaded) {
            $0.attachmentPicker.entries[0].status = .uploaded("stored-proof")
            $0.attachments = [NewRequestAttachment(id: id, name: draft.name, data: draft.data, storedID: "stored-proof")]
        }
        await store.receive(\.attachmentPicker.uploadNext)
        await store.send(.submitTapped) { $0.isSubmitting = true }
        await store.receive(\.submitFinished) {
            $0.isSubmitting = false
            $0.text = ""
            $0.attachments = []
            $0.attachmentPicker = AttachmentFeature.State()
            $0.successfulSubmissionCount = 1
        }
        #expect(await sent.body?.attachments == ["stored-proof"])
        await store.finish()
    }

    private func operations(
        options: @escaping @Sendable (String?) async throws -> OptionsCatalog = { _ in throw SubmissionFailure.refused },
        upload: @escaping @Sendable (NewRequestAttachment) async throws -> String = { _ in "stored" },
        create: @escaping @Sendable (NewRequestBody) async -> CreateRequestResult = { _ in .confirmed }
    ) -> NewWorkOperations {
        NewWorkOperations(
            projects: { [] }, options: options,
            issues: { _ in try JSONDecoder().decode(IssueList.self, from: Data(#"{"issues":[]}"#.utf8)) },
            captureSubmissionScope: { NewWorkSubmissionScope(upload: upload, isCurrent: { true }, create: create) }
        )
    }
}

private enum SubmissionFailure: Error, LocalizedError {
    case refused
    var errorDescription: String? { "refused" }
}

private actor SentBody {
    private(set) var body: NewRequestBody?
    func record(_ body: NewRequestBody) { self.body = body }
}

private actor SubmissionGate {
    private var revision = "A"
    private var checks = 0
    private(set) var dispatchedSigner: String?
    var isA: Bool { revision == "A" }
    func rotateToB() { revision = "B" }
    func checkThenRotateOnFirstCall() -> Bool {
        checks += 1
        let wasA = revision == "A"
        if checks == 1 { revision = "B" }
        return wasA
    }
    func recordDispatch(signer: String) { dispatchedSigner = signer }
}
