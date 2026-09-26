import AlondraQuestionsFeature
import AlondraShared
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct QuestionsFeatureTests {
    private func question(_ id: Int = 1, origin: String = "permission", options: String = "[]", toolUseID: String? = nil) throws -> Question {
        let marker = toolUseID.map { #", "toolUseId":"\#($0)""# } ?? ""
        let json = #"{"id":\#(id),"sessionId":"s","title":"Can I proceed?","options":\#(options),"links":[],"attachments":[],"origin":"\#(origin)","status":"open","askedAt":1\#(marker)}"#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(Question.self, from: Data(json.utf8))
    }

    private func report(_ id: Int = 7, links: String = "[]", attachments: String = "[]", body: String = "Text") throws -> Report {
        let json = #"{"id":\#(id),"sessionId":"s","title":"Document","body":"\#(body)","links":\#(links),"attachments":\#(attachments),"kind":"document","createdAt":1}"#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(Report.self, from: Data(json.utf8))
    }

    @Test
    func optionTapRequestsMissingExplanationThenSubmitsCompleteAnswerWithoutExtraSendTap() async throws {
        let q = try question(options: #"[{"label":"Deny","description":"Write the reason"}]"#)
        let store = TestStore(initialState: QuestionFeature.State(question: q)) { QuestionFeature() } withDependencies: {
            $0.questionWorkService = QuestionWorkService(submit: { operation in
                #expect(operation == .answer(id: 1, text: "Keep existing files", option: "Deny", attachments: []))
                return .confirmed
            }, document: { _ in "" })
        }
        await store.send(.optionTapped("Deny")) {
            $0.draft.selectedOption = "Deny"
            $0.replyFocusRequest = 1
        }
        await store.send(.textChanged("Keep existing files")) { $0.draft.text = "Keep existing files" }
        await store.send(.optionTapped("Deny"))
        await store.receive(\.answerTapped) { $0.isSubmitting = true }
        await store.receive(\.submissionFinished) {
            $0.isSubmitting = false
            $0.completed = true
            $0.draft = QuestionDraft()
        }
    }

    @Test
    func explanationOptionCannotSubmitWithoutExplanationAndKeepsDraftOnConflict() async throws {
        let q = try question(options: #"[{"label":"Deny","description":"Write the reason"}]"#)
        let store = TestStore(initialState: QuestionFeature.State(question: q)) { QuestionFeature() } withDependencies: {
            $0.questionWorkService = QuestionWorkService(submit: { _ in .conflict("Already answered on Mac") }, document: { _ in "" })
        }
        await store.send(.optionSelected("Deny")) { $0.draft.selectedOption = "Deny" }
        await store.send(.answerTapped) { $0.notice = "Explain your choice before sending it." }
        await store.send(.textChanged("A reason")) { $0.draft.text = "A reason" }
        await store.send(.answerTapped) {
            $0.isSubmitting = true
            $0.notice = nil
        }
        await store.receive(\.submissionFinished) {
            $0.isSubmitting = false
            $0.conflict = "Already answered on Mac"
        }
        #expect(store.state.draft.text == "A reason")
        #expect(!store.state.completed)
    }

    @Test
    func refreshRetainsDraftButSwitchingCardOrIdentityDoesNot() async throws {
        let first = try question()
        let second = try question(2)
        let store = TestStore(initialState: QuestionFeature.State(question: first)) { QuestionFeature() }
        await store.send(.textChanged("my reasoning")) { $0.draft.text = "my reasoning" }
        await store.send(.refreshed(first))
        await store.send(.refreshed(second)) {
            $0 = QuestionFeature.State(question: second)
            $0.revision = 1
        }
        await store.send(.textChanged("another device")) { $0.draft.text = "another device" }
        await store.send(.identityChanged) {
            $0.draft = QuestionDraft()
            $0.revision = 2
        }
        await store.send(.submissionFinished(0, .confirmed))
        #expect(!store.state.completed)
    }

    @Test
    func reportFeedbackRequiresContentAndFormatsAnchoredComments() throws {
        let report = try report()
        let useCase = ReviewDocumentUseCase()
        #expect(throws: QuestionWorkError.noFeedback) {
            try useCase.feedback(for: report, comments: [], general: "  ", author: "David", attachmentIDs: [])
        }
        let comment = ReviewComment(anchor: "p1", quote: "The design is unsafe", text: "Please revise")
        let request = try useCase.feedback(for: report, comments: [comment], general: "", author: "David", attachmentIDs: ["stored-1"])
        #expect(request.reportID == 7 && request.sessionID == "s")
        #expect(request.text.contains("Please revise"))
        #expect(request.attachmentIDs == ["stored-1"])
    }

    @Test
    func planChangesNeedFeedbackButApprovalCanUseProtocolOption() throws {
        let plan = try question(origin: "plan")
        var draft = QuestionDraft()
        draft.selectedOption = AnswerOption.requestChanges
        #expect(throws: QuestionWorkError.explanationRequired) {
            try AnswerQuestionUseCase().answer(plan, draft: draft)
        }
        draft.text = "Adjust the rollout order"
        #expect(try AnswerQuestionUseCase().answer(plan, draft: draft) ==
                .answer(id: 1, text: "Adjust the rollout order", option: AnswerOption.requestChanges, attachments: []))
        draft.selectedOption = AnswerOption.approve
        draft.text = ""
        #expect(try AnswerQuestionUseCase().answer(plan, draft: draft) ==
                .answer(id: 1, text: AnswerOption.approve, option: AnswerOption.approve, attachments: []))
    }

    @Test
    func terminalMenuRequiresExactOptionAndRejectsAttachmentsAndContext() throws {
        let menu = try question(options: #"[{"label":"Keep","description":null},{"label":"Close","description":null}]"#,
                                toolUseID: "menu:node-a")
        #expect(menu.isTerminalMenu)
        var draft = QuestionDraft()
        draft.text = "Close"
        #expect(throws: QuestionWorkError.menuRequiresOption) {
            try AnswerQuestionUseCase().answer(menu, draft: draft)
        }
        draft.selectedOption = "Close"
        draft.attachmentIDs = ["file-id"]
        #expect(throws: QuestionWorkError.menuDoesNotAcceptAttachments) {
            try AnswerQuestionUseCase().answer(menu, draft: draft)
        }
        draft.attachmentIDs = []
        #expect(try AnswerQuestionUseCase().answer(menu, draft: draft) ==
                .answer(id: 1, text: "Close", option: "Close", attachments: []))
        #expect(throws: QuestionWorkError.menuRequiresOption) {
            try AnswerQuestionUseCase().checkedMoreContext(menu, draft: draft)
        }
    }

    @Test
    func documentRefreshKeepsCommentsAndReloadsOnlyChangedSource() async throws {
        let report = try report()
        let store = TestStore(initialState: DocumentReviewFeature.State(subject: .report(report))) { DocumentReviewFeature() }
        await store.send(.commentChanged(anchor: "p1", quote: "quote", text: "Change this")) {
            $0.draft.comments = [ReviewComment(anchor: "p1", quote: "quote", text: "Change this")]
        }
        await store.send(.refreshed(.report(report)))
        #expect(store.state.draft.comments.first?.text == "Change this")
        let changed = try self.report(body: "Updated content")
        await store.send(.refreshed(.report(changed))) {
            $0.subject = .report(changed)
            $0.draft.comments = []
            $0.notice = "The document changed. Review it again before sending comments."
            $0.isLoading = true
            $0.loadRevision = 1
            $0.sections = [DocumentSection(source: .inline("Updated content"), text: "Updated content")]
        }
        await store.receive(\.loaded) {
            $0.isLoading = false
            $0.sourceText = "Updated content"
            $0.sections = [DocumentSection(source: .inline("Updated content"), text: "Updated content")]
        }
    }

    @Test
    func documentReviewLoadsEveryStoredAttachmentAlongsideReportBody() async throws {
        let report = try report(attachments: #"[{"name":"first.md","sourcePath":"/a","storedPath":"a"},{"name":"image.png","sourcePath":"/image","storedPath":"image"},{"name":"second.md","sourcePath":"/b","storedPath":"b"}]"#)
        let store = TestStore(initialState: DocumentReviewFeature.State(subject: .report(report))) { DocumentReviewFeature() } withDependencies: {
            $0.questionWorkService = QuestionWorkService(
                submit: { _ in .confirmed },
                document: { path in
                    if path == "a" { return "First content" }
                    if path == "b" { return "Second content" }
                    Issue.record("Binary image was fetched as text")
                    return "invalid"
                }
            )
        }
        await store.send(.appeared) {
            $0.isLoading = true
            $0.loadRevision = 1
            $0.sections = [DocumentSection(source: .inline("Text"), text: "Text"), DocumentSection(source: .stored(name: "first.md", path: "a"), text: "", isLoading: true), DocumentSection(source: .stored(name: "second.md", path: "b"), text: "", isLoading: true)]
        }
        await store.receive(\.sectionLoaded) { $0.sections[1].isLoading = false; $0.sections[1].text = "First content" }
        await store.receive(\.sectionLoaded) { $0.sections[2].isLoading = false; $0.sections[2].text = "Second content" }
        await store.receive(\.loaded) {
            $0.isLoading = false
            $0.sourceText = "Text\n\n---\n\n# first.md\n\nFirst content\n\n---\n\n# second.md\n\nSecond content"
            $0.sections = [DocumentSection(source: .inline("Text"), text: "Text"), DocumentSection(source: .stored(name: "first.md", path: "a"), text: "First content"), DocumentSection(source: .stored(name: "second.md", path: "b"), text: "Second content")]
        }
    }

    @Test
    func phoneDocumentLoadsOnlyOriginalStoredSourceAndKeepsAnchorsWhenHiddenBodyChanges() async throws {
        let attachments = #"[{"name":"local.md","sourcePath":"/local"},{"name":"first.md","sourcePath":"/a","storedPath":"a"},{"name":"second.md","sourcePath":"/b","storedPath":"b"}]"#
        let original = try report(attachments: attachments, body: "Report summary")
        let text = "## Original heading\n\nThe paragraph to review."
        let source = DocumentSource.stored(name: "first.md", path: "a")
        let store = TestStore(initialState: DocumentReviewFeature.State(subject: .report(original))) { DocumentReviewFeature() } withDependencies: {
            $0.questionWorkService = QuestionWorkService(submit: { _ in .confirmed }, document: { path in
                #expect(path == "a")
                return text
            })
        }
        await store.send(.phoneAppeared) {
            $0.sourcePolicy = .phone
            $0.isLoading = true
            $0.loadRevision = 1
            $0.sections = [DocumentSection(source: source, text: "", isLoading: true)]
        }
        await store.receive(\.sectionLoaded) {
            $0.sections[0].isLoading = false
            $0.sections[0].text = text
        }
        await store.receive(\.loaded) {
            $0.isLoading = false
            $0.sourceText = text
        }
        let anchor = try #require(MarkdownParser.blocks(from: text).last)
        await store.send(.commentChanged(anchor: anchor.draftKey, quote: anchor.plainText, text: "Please revise")) {
            $0.draft.comments = [ReviewComment(anchor: anchor.draftKey, quote: anchor.plainText, text: "Please revise")]
        }
        let refreshed = try report(attachments: attachments, body: "Updated summary")
        await store.send(.refreshed(.report(refreshed))) { $0.subject = .report(refreshed) }
        #expect(store.state.sourceText == text)
        #expect(store.state.draft.comments.first?.anchor == MarkdownParser.blocks(from: text).last?.draftKey)
        await store.finish()
    }

    @Test
    func phonePlanUsesDetailVerbatimEvenWithStoredAttachments() async throws {
        let json = ###"{"id":1,"sessionId":"s","title":"Plan","detail":"## Plan","options":[],"links":[],"attachments":[{"name":"other.md","sourcePath":"/a","storedPath":"a"}],"origin":"plan","status":"open","askedAt":1}"###
        let plan = try JSONDecoder().decode(Question.self, from: Data(json.utf8))
        let store = TestStore(initialState: DocumentReviewFeature.State(subject: .question(plan))) { DocumentReviewFeature() } withDependencies: {
            $0.questionWorkService = QuestionWorkService(submit: { _ in .confirmed }, document: { _ in
                Issue.record("Phone plan review must not fetch attachments when detail is present")
                return "Unexpected"
            })
        }
        await store.send(.phoneAppeared) {
            $0.sourcePolicy = .phone
            $0.isLoading = true
            $0.loadRevision = 1
            $0.sections = [DocumentSection(source: .inline("## Plan"), text: "## Plan")]
        }
        await store.receive(\.loaded) {
            $0.isLoading = false
            $0.sourceText = "## Plan"
        }
        await store.finish()
    }

    @Test
    func failedReloadThenChangedContentClearsOldAnchoredComments() async throws {
        let report = try report(attachments: #"[{"name":"draft.md","sourcePath":"/a","storedPath":"a"}]"#)
        let reader = ChangingDocument()
        let store = TestStore(initialState: DocumentReviewFeature.State(subject: .report(report))) { DocumentReviewFeature() } withDependencies: {
            $0.questionWorkService = QuestionWorkService(submit: { _ in .confirmed }, document: { _ in try await reader.read() })
        }
        await store.send(.appeared) { $0.isLoading = true; $0.loadRevision = 1; $0.sections = [DocumentSection(source: .inline("Text"), text: "Text"), DocumentSection(source: .stored(name: "draft.md", path: "a"), text: "", isLoading: true)] }
        await store.receive(\.sectionLoaded) { $0.sections[1].isLoading = false; $0.sections[1].text = "Old" }
        await store.receive(\.loaded) {
            $0.isLoading = false
            $0.sourceText = "Text\n\n---\n\n# draft.md\n\nOld"
            $0.sections = [DocumentSection(source: .inline("Text"), text: "Text"), DocumentSection(source: .stored(name: "draft.md", path: "a"), text: "Old")]
        }
        await store.send(.commentChanged(anchor: "p1", quote: "Old", text: "Fix this")) {
            $0.draft.comments = [ReviewComment(anchor: "p1", quote: "Old", text: "Fix this")]
        }
        await store.send(.appeared) {
            $0.isLoading = true
            $0.sourceText = nil
            $0.sections = [DocumentSection(source: .inline("Text"), text: "Text"), DocumentSection(source: .stored(name: "draft.md", path: "a"), text: "", isLoading: true)]
            $0.previousTextForComparison = "Text\n\n---\n\n# draft.md\n\nOld"
            $0.loadRevision = 2
        }
        await store.receive(\.sectionLoaded) { $0.sections[1].isLoading = false; $0.sections[1].error = QuestionWorkError.unavailable.localizedDescription }
        await store.receive(\.loaded) {
            $0.isLoading = false
            $0.loadError = QuestionWorkError.unavailable.localizedDescription
        }
        await store.send(.appeared) {
            $0.isLoading = true
            $0.loadError = nil
            $0.loadRevision = 3
            $0.sections = [DocumentSection(source: .inline("Text"), text: "Text"), DocumentSection(source: .stored(name: "draft.md", path: "a"), text: "", isLoading: true)]
        }
        await store.receive(\.sectionLoaded) { $0.sections[1].isLoading = false; $0.sections[1].text = "New" }
        await store.receive(\.loaded) {
            $0.isLoading = false
            $0.sourceText = "Text\n\n---\n\n# draft.md\n\nNew"
            $0.sections = [DocumentSection(source: .inline("Text"), text: "Text"), DocumentSection(source: .stored(name: "draft.md", path: "a"), text: "New")]
            $0.previousTextForComparison = nil
            $0.draft.comments = []
            $0.notice = "The document changed. Review it again before sending comments."
        }
    }

    @Test
    func recreatedStoreUsesSemanticFeedbackServiceWithoutResendingMessage() async throws {
        let report = try report()
        let identity = QuestionWorkIdentity(server: URL(string: "https://relay.example")!, deviceID: "phone", credentialRevision: "key-a", generation: 1)
        let coordinator = DurableFeedbackFake()
        var initial = DocumentReviewFeature.State(subject: .report(report), identity: identity)
        initial.sourceText = "Text"
        let store = TestStore(initialState: initial) { DocumentReviewFeature() } withDependencies: {
            $0.questionWorkService = QuestionWorkService(
                submit: { _ in .confirmed },
                submitReportFeedback: { request, scope in await coordinator.submit(request, scope: scope) },
                document: { _ in "" }
            )
        }
        await store.send(.generalChanged("A change is needed")) { $0.draft.general = "A change is needed" }
        await store.send(.sendFeedbackTapped(author: "David")) { $0.isSubmitting = true }
        await store.receive(\.reportFeedbackFinished) {
            $0.isSubmitting = false
            $0.needsReconciliation = true
            $0.notice = "The result is uncertain. Check the server before trying again."
        }
        #expect(await coordinator.messageSends == 1)
        #expect(!store.state.completed)

        // A relaunch destroys reducer state. The process-owned durable service, not this Store,
        // must resume its saved "message sent, read pending" transaction.
        let relaunched = TestStore(initialState: initial) { DocumentReviewFeature() } withDependencies: {
            $0.questionWorkService = QuestionWorkService(
                submit: { _ in .confirmed },
                submitReportFeedback: { request, scope in await coordinator.submit(request, scope: scope) },
                document: { _ in "" }
            )
        }
        await relaunched.send(.generalChanged("A change is needed")) { $0.draft.general = "A change is needed" }
        await relaunched.send(.sendFeedbackTapped(author: "David")) {
            $0.isSubmitting = true
        }
        await relaunched.receive(\.reportFeedbackFinished) {
            $0.isSubmitting = false
            $0.completed = true
            $0.draft = DocumentDraft()
        }
        #expect(await coordinator.submitCalls == 2)
        #expect(await coordinator.messageSends == 1)
        #expect(await coordinator.readAttempts == 2)
    }

    @Test
    func reviewActionsAreDisabledDuringReloadAndAfterLoadFailure() async throws {
        let report = try report(links: #"[{"url":"https://claude.ai/artifact/example123"}]"#)
        var loading = DocumentReviewFeature.State(subject: .report(report))
        loading.sourceText = "Stale content"
        loading.isLoading = true
        loading.loadRevision = 1
        loading.draft.general = "Approve this"
        let store = TestStore(initialState: loading) { DocumentReviewFeature() }
        await store.send(.sendFeedbackTapped(author: "David"))
        await store.send(.reviewTapped(.approved))
        await store.send(.loaded(1, .failure(DocumentLoadFailure(message: "Document unavailable")))) {
            $0.isLoading = false
            $0.loadError = "Document unavailable"
        }
        await store.send(.sendFeedbackTapped(author: "David"))
        await store.send(.reviewTapped(.approved))
        #expect(!store.state.isSubmitting)
    }

    @Test
    func uncertainQuestionNeedsExplicitReconciliationAndEditingDuringSendIsBlocked() async throws {
        let q = try question()
        let (results, continuation) = AsyncStream<QuestionWorkOutcome>.makeStream()
        let store = TestStore(initialState: QuestionFeature.State(question: q)) { QuestionFeature() } withDependencies: {
            $0.questionWorkService = QuestionWorkService(submit: { _ in
                for await result in results { return result }
                return .obsolete
            }, document: { _ in "" })
        }
        await store.send(.textChanged("Original")) { $0.draft.text = "Original" }
        await store.send(.answerTapped) { $0.isSubmitting = true }
        await store.send(.textChanged("Late edit"))
        continuation.yield(.uncertain)
        continuation.finish()
        await store.receive(\.submissionFinished) {
            $0.isSubmitting = false
            $0.needsReconciliation = true
            $0.notice = "The result is uncertain. Check the server before trying again."
        }
        await store.send(.answerTapped)
        await store.send(.reconciled(cardStillOpen: true)) {
            $0.needsReconciliation = false
            $0.notice = nil
        }
        #expect(store.state.draft.text == "Original")
        #expect(!store.state.completed)
    }
}

private actor DurableFeedbackFake {
    private var sentFor: (QuestionWorkIdentity, UInt64)?
    var submitCalls = 0
    var messageSends = 0
    var readAttempts = 0

    func submit(_ request: ReportFeedbackRequest, scope: QuestionWorkIdentity) -> QuestionWorkOutcome {
        submitCalls += 1
        if sentFor == nil {
            sentFor = (scope, request.reportID)
            messageSends += 1
        } else if sentFor?.0 != scope || sentFor?.1 != request.reportID {
            return .obsolete
        }
        readAttempts += 1
        return readAttempts == 1 ? .uncertain : .confirmed
    }
}

private actor ChangingDocument {
    private var reads = 0
    func read() throws -> String {
        reads += 1
        if reads == 2 { throw QuestionWorkError.unavailable }
        return reads == 1 ? "Old" : "New"
    }
}
