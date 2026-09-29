import AlondraAttachmentsFeature
import AlondraDomain
import AlondraPullRequestsFeature
import AlondraQuestionsFeature
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct ReviewUploadNoticeIntegrationTests {
    enum Route: CaseIterable { case question, document, ask, changes, attached }

    @Test(.timeLimit(.minutes(1)), arguments: Route.allCases, [false, true])
    func sendUploadFailureUsesOneToastWithoutDispatch(route: Route, inCard: Bool) async throws {
        try await verify(route: route, inCard: inCard, sending: true)
    }

    @Test(.timeLimit(.minutes(1)))
    func backgroundUploadFailureKeepsRetryChipWithoutSubmissionToast() async throws {
        try await verify(route: .question, inCard: true, sending: false)
    }

    private func verify(route: Route, inCard: Bool, sending: Bool) async throws {
        let file = AttachmentDraft(name: "proof.txt", data: Data("proof".utf8))
        var fixture = V2Fixture()
        let item: DomainV2.InboxItem = switch route {
        case .question: fixture.question("q1")
        case .document: fixture.documentReview("a")
        case .ask, .changes: fixture.pullRequest(5)
        case .attached: fixture.pullRequest(5, questions: ["q1"])
        }
        var card = TriageCardFeature.State(item: item, workspace: fixture.workspace, identity: V2Fixture.workIdentity)
        let entries = [AttachmentFeature.Entry(draft: file, status: .failed("Offline"))]
        switch route {
        case .question:
            card.question?.draft.selectedOption = "reply"
            card.question?.draft.text = "Keep this draft"
            card.question?.attachmentPicker.entries = entries
        case .document:
            card.document?.sourceText = "Body"
            card.document?.draft.general = "Keep this draft"
            card.document?.attachmentPicker.entries = entries
        case .ask, .changes:
            card.pullRequest?.diff.files = []
            card.pullRequest?.diff.note = "Keep this draft"
            card.pullRequest?.diff.loadedDiffText = ""
            card.pullRequest?.diff.loadedHeadSHA = "head"
            card.pullRequest?.diff.attachmentPicker.entries = entries
        case .attached:
            card.pullRequest?.questionFeatures[id: "q1"]?.question.draft.selectedOption = "reply"
            card.pullRequest?.questionFeatures[id: "q1"]?.question.draft.text = "Keep this draft"
            card.pullRequest?.questionFeatures[id: "q1"]?.question.attachmentPicker.entries = entries
        }
        let cardID = card.id
        let documentID = card.document?.id ?? cardID
        var initial = ServiceRootFeature.State()
        if inCard { initial.triage.cards = [card] }
        else {
            initial.triage.question = card.question
            initial.triage.document = card.document
            initial.pullRequest = card.pullRequest
        }
        func action(_ child: TriageCardFeature.Action) -> ServiceRootFeature.Action {
            if inCard { return .triage(.cards(.element(id: cardID, action: child))) }
            switch child {
            case .question(let value): return .triage(.question(.presented(value)))
            case .document(let value): return .triage(.documents(.element(id: documentID, action: value)))
            case .pullRequest(let value): return .pullRequests(.element(id: "pr-5", action: value))
            default: preconditionFailure("Expected feature action")
            }
        }
        func uploadAction(_ event: AttachmentFeature.Action) -> ServiceRootFeature.Action {
            switch route {
            case .question: return action(.question(.attachmentPicker(event)))
            case .document: return action(.document(.attachmentPicker(event)))
            case .ask, .changes: return action(.pullRequest(.diff(.attachmentPicker(event))))
            case .attached: return action(.pullRequest(.attachedQuestion(.element(id: "q1", action: .question(.attachmentPicker(event))))))
            }
        }
        let submit: ServiceRootFeature.Action
        switch route {
        case .question: submit = action(.question(.answerTapped))
        case .document: submit = action(.document(.sendFeedbackTapped(author: "")))
        case .ask: submit = action(.pullRequest(.diff(.askTapped)))
        case .changes: submit = action(.pullRequest(.diff(.requestChangesTapped)))
        case .attached: submit = action(.pullRequest(.attachedQuestion(.element(id: "q1", action: .question(.answerTapped)))))
        }
        let uploads = LockIsolated<[AttachmentDraft]>([])
        let finals = CommandLog()
        let notices = LockIsolated<Set<UUID>>([])
        let observed = AsyncStream<Void>.makeStream()
        let clock = TestClock()
        let store = Store(initialState: initial) {
            CombineReducers {
                ServiceRootFeature()
                Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> { state, _ in
                    if let notice = state.notice { notices.withValue { $0.insert(notice.id) } }
                    let picker: AttachmentFeature.State?
                    let q = inCard ? state.triage.cards[id: cardID]?.question : state.triage.question
                    let d = inCard ? state.triage.cards[id: cardID]?.document : state.triage.document
                    let p = inCard ? state.triage.cards[id: cardID]?.pullRequest : state.pullRequest
                    switch route {
                    case .question: picker = q?.attachmentPicker
                    case .document: picker = d?.attachmentPicker
                    case .ask, .changes: picker = p?.diff.attachmentPicker
                    case .attached: picker = p?.questionFeatures[id: "q1"]?.question.attachmentPicker
                    }
                    if picker?.entries.first?.status == .failed("Still offline"), !sending || state.notice != nil {
                        observed.continuation.yield(())
                    }
                    return .none
                }
            }
        } withDependencies: {
            $0.continuousClock = clock
            $0.attachmentUpload = .init(upload: { draft in uploads.withValue { $0.append(draft) }; throw AttachmentFailure(message: "Still offline") })
            $0.domainWorkspace = .scripted(finals, documents: ["rev-a": "Body"]) { _ in V2Fixture.rejected("Unexpected") }
        }
        // Enter the triage scope to mount the seeded @Presents lifetime separately.
        // A root-only action does not run this scoped reducer.
        let presentation = store.send(.triage(.machinePresented(false)))
        let operation = store.send(sending ? submit : uploadAction(.retry(file.id)))
        var iterator = observed.stream.makeAsyncIterator()
        _ = await iterator.next()
        #expect(uploads.value == [file])
        #expect(finals.count == 0)
        #expect(notices.value.count == (sending ? 1 : 0))
        if sending { #expect(store.notice?.event == .failure(couldNotSendMessage("Still offline"))) }
        let q = inCard ? store.triage.cards[id: cardID]?.question : store.triage.question
        let d = inCard ? store.triage.cards[id: cardID]?.document : store.triage.document
        let p = inCard ? store.triage.cards[id: cardID]?.pullRequest : store.pullRequest
        switch route {
        case .question:
            let state = try #require(q)
            #expect(state.pendingSubmission == nil && state.draft.text == "Keep this draft")
            #expect(state.noticeIsSubmissionFailure == sending)
            #expect(state.attachmentPicker.entries.first?.draft == file)
        case .document:
            let state = try #require(d)
            #expect(state.pendingReview == nil && state.draft.general == "Keep this draft")
            #expect(state.noticeIsSubmissionFailure)
            #expect(state.attachmentPicker.entries.first?.draft == file)
        case .ask, .changes:
            let state = try #require(p?.diff)
            #expect(state.pendingReviewKind == nil && state.note == "Keep this draft")
            #expect(state.validationErrorIsSubmissionFailure)
            #expect(state.attachmentPicker.entries.first?.draft == file)
        case .attached:
            let state = try #require(p?.questionFeatures[id: "q1"]?.question)
            #expect(state.pendingSubmission == nil && !state.isSubmitting)
            #expect(state.draft.text == "Keep this draft")
            #expect(state.attachmentPicker.entries.first?.draft == file)
        }
        await clock.advance(by: .seconds(6))
        await operation.finish()
        #expect(store.notice == nil && notices.value.count == (sending ? 1 : 0))
        await store.send(uploadAction(.uploaded(file.id, .failure(.init(message: "Duplicate"))))).finish()
        #expect(store.notice == nil && finals.count == 0)
        if !inCard, route == .question {
            await store.send(.triage(.question(.dismiss))).finish()
        }
        await presentation.finish()
        observed.continuation.finish()
    }
}
