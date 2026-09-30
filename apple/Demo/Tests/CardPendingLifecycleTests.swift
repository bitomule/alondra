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
struct CardPendingLifecycleTests {
    enum Surface: CaseIterable { case question, document, diff, attachedQuestion }
    enum Departure: CaseIterable { case none, tab, scene, rootScene, skip, focus, skipGesture, acceptedSwipe, rejectedSwipe }

    nonisolated static let receipt = DomainV2.AttachmentRef(id: "stored-proof", name: "proof.txt", contentDigest: Data("proof".utf8).sha256Hex,
                                                            contentReference: "ref", bytes: 5)

    @Test(arguments: Surface.allCases, Departure.allCases)
    func lateReceiptCannotSubmitAfterLeavingCard(surface: Surface, departure: Departure) async throws {
        var fixture = V2Fixture()
        let item: DomainV2.InboxItem = switch surface {
        case .question: fixture.question("q1")
        case .document: fixture.documentReview("a")
        case .diff: fixture.pullRequest(5)
        case .attachedQuestion: fixture.pullRequest(5, questions: ["q1"])
        }
        let next = fixture.information("next", title: "Next", order: 100)
        let workspace = fixture.workspace
        let attachment = AttachmentDraft(id: UUID(uuidString: "00000000-0000-0000-0000-000000000091")!, name: "proof.txt", data: Data("proof".utf8))
        var picker = AttachmentFeature.State()
        picker.entries = [.init(draft: attachment, status: .uploading)]
        var card = TriageCardFeature.State(item: item, workspace: workspace, identity: V2Fixture.workIdentity)
        switch surface {
        case .question:
            card.question?.draft.selectedOption = "reply"
            card.question?.draft.text = "Keep this draft"
            card.question?.attachmentPicker = picker
            card.question?.pendingSubmission = .answer
        case .document:
            card.document?.sourceText = "Body"
            card.document?.draft.general = "Keep this draft"
            card.document?.attachmentPicker = picker
            card.document?.pendingReview = .feedback("Keep this draft")
        case .diff:
            card.pullRequest?.diff.note = "Keep this draft"
            card.pullRequest?.diff.files = []
            card.pullRequest?.diff.loadedDiffText = ""
            card.pullRequest?.diff.loadedHeadSHA = V2Fixture.head
            card.pullRequest?.diff.attachmentPicker = picker
            card.pullRequest?.diff.pendingReviewKind = .ask
        case .attachedQuestion:
            card.pullRequest?.questionFeatures[id: "q1"]?.question.draft.selectedOption = "reply"
            card.pullRequest?.questionFeatures[id: "q1"]?.question.draft.text = "Keep this draft"
            card.pullRequest?.questionFeatures[id: "q1"]?.question.attachmentPicker = picker
            card.pullRequest?.questionFeatures[id: "q1"]?.question.pendingSubmission = .answer
        }
        var initial = ServiceRootFeature.State()
        initial.triage.triage.workspace = workspace
        initial.triage.triage.items = [item, next]
        initial.triage.triage.cursor = item.place
        initial.triage.cards = [card, TriageCardFeature.State(item: next, workspace: workspace, identity: V2Fixture.workIdentity)]
        let log = CommandLog()
        let store = Store(initialState: initial) { ServiceRootFeature() } withDependencies: {
            $0.domainWorkspace = .scripted(log) { intent in
                switch intent {
                case .answer(_, _, _, let attachments), .submitFeedback(_, _, let attachments), .askAboutPullRequest(_, _, _, let attachments):
                    #expect(attachments == [Self.receipt])
                default: Issue.record("Unexpected command \(intent)")
                }
                return .obsolete
            }
        }
        // Each departure is reduced synchronously before releasing a receipt. No
        // disappearance callback or queued child cancellation is needed for safety.
        switch departure {
        case .none: break
        case .tab: await store.send(.tabSelected(.tasks)).finish()
        case .scene: await store.send(.triage(.triage(.disappeared))).finish()
        case .rootScene: await store.send(.disappeared).finish()
        case .skip: await store.send(.triage(.triage(.skip(item.id)))).finish()
        case .focus: await store.send(.triage(.triage(.focus(next.id)))).finish()
        case .skipGesture: await store.send(.triage(.cardStack(.skipTapped(item.id)))).finish()
        case .acceptedSwipe, .rejectedSwipe:
            await store.send(.triage(.cardStack(.dragged(width: departure == .acceptedSwipe ? 110 : 20, height: 0)))).finish()
            await store.send(.triage(.cardStack(.dragEnded(projectedWidth: 0)))).finish()
        }
        if departure == .skip || departure == .focus {
            #expect(store.triage.triage.queue.first?.id == next.id)
        }
        if departure == .skipGesture || departure == .acceptedSwipe {
            #expect(store.triage.cardStack.departure?.itemID == item.id)
            #expect(store.triage.triage.queue.first?.id == item.id, "Receipt arrives before animationCompleted")
        } else if departure == .rejectedSwipe {
            #expect(store.triage.cardStack.departure == nil)
        }
        let receipt = AttachmentFeature.Action.uploaded(attachment.id, .success(Self.receipt))
        let action: TriageCardFeature.Action = switch surface {
        case .question: .question(.attachmentPicker(receipt))
        case .document: .document(.attachmentPicker(receipt))
        case .diff: .pullRequest(.diff(.attachmentPicker(receipt)))
        case .attachedQuestion: .pullRequest(.attachedQuestion(.element(id: "q1", action: .question(.attachmentPicker(receipt)))))
        }
        await store.send(.triage(.cards(.element(id: item.id, action: action)))).finish()
        await store.send(.triage(.cards(.element(id: item.id, action: action)))).finish()
        #expect(log.count == ((departure == .none || departure == .rejectedSwipe) ? 1 : 0))
        let retained = try #require(store.triage.cards[id: item.id])
        let retainedPicker: AttachmentFeature.State?
        let retainedDraft: String?
        switch surface {
        case .question: retainedPicker = retained.question?.attachmentPicker; retainedDraft = retained.question?.draft.text
        case .document: retainedPicker = retained.document?.attachmentPicker; retainedDraft = retained.document?.draft.general
        case .diff: retainedPicker = retained.pullRequest?.diff.attachmentPicker; retainedDraft = retained.pullRequest?.diff.note
        case .attachedQuestion:
            retainedPicker = retained.pullRequest?.questionFeatures[id: "q1"]?.question.attachmentPicker
            retainedDraft = retained.pullRequest?.questionFeatures[id: "q1"]?.question.draft.text
        }
        #expect(retainedDraft == "Keep this draft")
        #expect(retainedPicker?.stored == [Self.receipt])
        #expect(retainedPicker?.entries.first?.draft == attachment)
    }

    enum Navigation: CaseIterable { case question, information, node, task, request, schedules, cleared, inboxChanged, unchanged }

    @Test(arguments: Navigation.allCases)
    func changingSelectionCancelsPresentedPRBeforeNavigationEffects(navigation: Navigation) async throws {
        var fixture = V2Fixture()
        let question = fixture.question("q1")
        let information = fixture.information("info-99", title: "Report", order: 100)
        let pr = fixture.pullRequest(5, questions: ["q5"])
        let workspace = fixture.workspace
        let attachment = AttachmentDraft(id: UUID(uuidString: "00000000-0000-0000-0000-000000000093")!, name: "proof.txt", data: Data("proof".utf8))
        var picker = AttachmentFeature.State()
        picker.entries = [.init(draft: attachment, status: .uploading)]
        var detail = try #require(TriageCardFeature.State(item: pr, workspace: workspace, identity: nil).pullRequest)
        detail.diff.note = "Keep review"
        detail.diff.files = []
        detail.diff.loadedDiffText = ""
        detail.diff.loadedHeadSHA = V2Fixture.head
        detail.diff.attachmentPicker = picker
        detail.diff.pendingReviewKind = .ask
        detail.questionFeatures[id: "q5"]?.question.draft.selectedOption = "reply"
        detail.questionFeatures[id: "q5"]?.question.draft.text = "Keep answer"
        detail.questionFeatures[id: "q5"]?.question.attachmentPicker = picker
        detail.questionFeatures[id: "q5"]?.question.pendingSubmission = .answer
        var initial = ServiceRootFeature.State()
        initial.pullRequest = detail
        initial.macPresentation.selection = .viewKey("pull_request:pr-5")
        initial.triage.triage.workspace = workspace
        initial.triage.triage.items = [question, information]
        let log = CommandLog()
        let store = Store(initialState: initial) { ServiceRootFeature() } withDependencies: {
            $0.domainWorkspace = .scripted(log) { _ in .obsolete }
        }
        let action: ServiceRootFeature.Action = switch navigation {
        case .question: .focusInbox(question.id)
        case .information: .focusInbox(information.id)
        case .node: .macPresentation(.select(.viewKey("node:node")))
        case .task: .macPresentation(.select(.viewKey("task:task")))
        case .request: .macPresentation(.select(.viewKey("request:request")))
        case .schedules: .macPresentation(.select(.viewKey("schedules")))
        case .cleared: .macPresentation(.select(nil))
        case .inboxChanged: .macPresentation(.select(.viewKey("card:q1")))
        case .unchanged: .macPresentation(.select(.viewKey("pull_request:pr-5")))
        }
        let transition = store.send(action)
        // Inspect before awaiting the queued focus/open effects or a SwiftUI callback.
        #expect((store.pullRequests[id: "pr-5"]?.diff.pendingReviewKind != nil) == (navigation == .unchanged))
        #expect((store.pullRequests[id: "pr-5"]?.questionFeatures[id: "q5"]?.question.pendingSubmission != nil) == (navigation == .unchanged))
        let receipt = AttachmentFeature.Action.uploaded(attachment.id, .success(Self.receipt))
        await store.send(.pullRequests(.element(id: "pr-5", action: .diff(.attachmentPicker(receipt))))).finish()
        await store.send(.pullRequests(.element(id: "pr-5", action: .attachedQuestion(.element(id: "q5", action: .question(.attachmentPicker(receipt))))))).finish()
        await transition.finish()
        #expect(log.count == (navigation == .unchanged ? 2 : 0))
        #expect(store.pullRequests[id: "pr-5"]?.diff.note == "Keep review")
        #expect(store.pullRequests[id: "pr-5"]?.diff.attachmentPicker.stored == [Self.receipt])
        #expect(store.pullRequests[id: "pr-5"]?.questionFeatures[id: "q5"]?.question.draft.text == "Keep answer")
        #expect(store.pullRequests[id: "pr-5"]?.questionFeatures[id: "q5"]?.question.attachmentPicker.stored == [Self.receipt])
        if navigation == .question || navigation == .information {
            #expect(store.triage.document?.subject.id == (navigation == .information ? information.id : question.id))
        }
        if navigation == .inboxChanged { #expect(store.macPresentation.selection == .viewKey("card:q1")) }
    }

    @Test func closingRootDoesNotCancelAlreadyDispatchedCommandOrLoseItsUnknownOutcome() async throws {
        var fixture = V2Fixture()
        let question = fixture.question("q1")
        var card = TriageCardFeature.State(item: question, workspace: fixture.workspace, identity: nil)
        card.question?.draft.selectedOption = "reply"
        card.question?.draft.text = "Already sent"
        var initial = ServiceRootFeature.State()
        initial.triage.triage.items = [question]
        initial.triage.cards = [card]
        let gate = CommandGate()
        let log = CommandLog()
        let store = Store(initialState: initial) { ServiceRootFeature() } withDependencies: {
            $0.domainWorkspace = .scripted(log) { _ in
                let result = await gate.run()
                #expect(!Task.isCancelled, "Leaving the scene must not cancel an already dispatched command")
                return result
            }
        }
        let submission = store.send(.triage(.cards(.element(id: question.id, action: .question(.answerTapped)))))
        await gate.waitUntilStarted()
        #expect(store.triage.cards[id: question.id]?.question?.isSubmitting == true)
        await store.send(.disappeared).finish()
        #expect(store.triage.cards[id: question.id]?.question?.isSubmitting == true)
        await gate.release(.uncertain)
        await submission.finish()
        let result = try #require(store.triage.cards[id: question.id]?.question)
        #expect(result.needsReconciliation)
        #expect(result.commandID != nil, "sending again reuses the unknown attempt's command ID")
        #expect(result.draft.text == "Already sent")
        #expect(!result.isSubmitting)
    }
}
