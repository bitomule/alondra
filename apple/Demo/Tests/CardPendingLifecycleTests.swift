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
    enum Surface: CaseIterable { case question, report, diff, attachedQuestion }
    enum Departure: CaseIterable { case none, tab, scene, rootScene, skip, focus, skipGesture, acceptedSwipe, rejectedSwipe }

    @Test(arguments: Surface.allCases, Departure.allCases)
    func lateReceiptCannotSubmitAfterLeavingCard(surface: Surface, departure: Departure) async throws {
        let decoder = JSONDecoder()
        let question = try decoder.decode(Question.self, from: Data(#"{"id":1,"sessionId":"s","title":"Proceed?","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","askedAt":1}"#.utf8))
        let report = try decoder.decode(Report.self, from: Data(#"{"id":7,"sessionId":"s","title":"Report","body":"Body","links":[],"attachments":[],"kind":"document","createdAt":1}"#.utf8))
        let next = try decoder.decode(Report.self, from: Data(#"{"id":99,"sessionId":"s","title":"Next","body":"Body","links":[],"attachments":[],"kind":"document","createdAt":100}"#.utf8))
        let pr = try decoder.decode(PullRequest.self, from: Data(#"{"id":5,"sessionId":"s","url":"https://example.com/p/5","repo":"a/b","number":5,"title":"PR","state":"open","openedAt":1}"#.utf8))
        let attachment = AttachmentDraft(id: UUID(uuidString: "00000000-0000-0000-0000-000000000091")!, name: "proof.txt", data: Data("proof".utf8))
        var picker = AttachmentFeature.State()
        picker.entries = [.init(draft: attachment, status: .uploading)]
        let identity = QuestionWorkIdentity(server: URL(string: "https://service.example")!, deviceID: "phone", credentialRevision: "one", generation: 1)
        let item: InboxItem = switch surface {
        case .question: .question(question)
        case .report: .report(report)
        case .diff, .attachedQuestion: .pullRequest(pr)
        }
        var card = TriageCardFeature.State(item: item, identity: identity)
        switch surface {
        case .question:
            card.question?.draft.text = "Keep this draft"
            card.question?.attachmentPicker = picker
            card.question?.pendingSubmission = .awaitingReceipts
        case .report:
            card.document?.sourceText = "Body"
            card.document?.draft.general = "Keep this draft"
            card.document?.attachmentPicker = picker
            card.document?.pendingSubmission = .awaitingReceipts
            card.document?.pendingReview = .feedback("David")
        case .diff:
            card.pullRequest?.diff.note = "Keep this draft"
            card.pullRequest?.diff.files = []
            card.pullRequest?.diff.loadedDiffText = ""
            card.pullRequest?.diff.loadedHeadSHA = "head"
            card.pullRequest?.diff.attachmentPicker = picker
            card.pullRequest?.diff.pendingSubmission = .awaitingReceipts
            card.pullRequest?.diff.pendingReviewKind = .ask
        case .attachedQuestion:
            var attached = AttachedPRQuestionFeature.State(question)
            attached.question.draft.text = "Keep this draft"
            attached.question.attachmentPicker = picker
            attached.question.pendingSubmission = .awaitingReceipts
            attached.question.pendingAttachedAnswer = attached.question.draft
            card.pullRequest?.questionFeatures = [attached]
        }
        var initial = ServiceRootFeature.State()
        initial.triage.triage.items = [item, .report(next)]
        initial.triage.triage.cursor = item.place
        initial.triage.cards = [card, TriageCardFeature.State(item: .report(next), identity: identity)]
        let sent = SubmissionEvidence()
        let store = Store(initialState: initial) { ServiceRootFeature() } withDependencies: {
            $0.questionWorkService = QuestionWorkService(submit: { _ in await sent.record(); return .obsolete },
                submitReportFeedback: { request, _ in
                    #expect(request.attachmentIDs == ["stored-proof"])
                    await sent.record(); return .obsolete
                }, document: { _ in "" })
            $0.pullRequestOperations = PullRequestOperations(
                diff: { _ in PRDiffSnapshot(diff: "", headSHA: "head") },
                merge: { _, _ in .obsolete }, resolveConflicts: { _ in .obsolete }, setAside: { _ in .obsolete },
                review: { _, _, attachments, _, _ in
                    #expect(attachments.map(\.storedID) == ["stored-proof"])
                    await sent.record(); return .obsolete
                })
        }
        // Each departure is reduced synchronously before releasing a receipt. No
        // disappearance callback or queued child cancellation is needed for safety.
        switch departure {
        case .none: break
        case .tab: await store.send(.tabSelected(.tasks)).finish()
        case .scene: await store.send(.triage(.triage(.disappeared))).finish()
        case .rootScene: await store.send(.disappeared).finish()
        case .skip: await store.send(.triage(.triage(.skip(item.id)))).finish()
        case .focus: await store.send(.triage(.triage(.focus("r99")))).finish()
        case .skipGesture: await store.send(.triage(.cardStack(.skipTapped(item.id)))).finish()
        case .acceptedSwipe, .rejectedSwipe:
            await store.send(.triage(.cardStack(.dragged(width: departure == .acceptedSwipe ? 110 : 20, height: 0)))).finish()
            await store.send(.triage(.cardStack(.dragEnded(projectedWidth: 0)))).finish()
        }
        if departure == .skip || departure == .focus {
            #expect(store.triage.triage.queue.first?.id == "r99")
        }
        if departure == .skipGesture || departure == .acceptedSwipe {
            #expect(store.triage.cardStack.departure?.itemID == item.id)
            #expect(store.triage.triage.queue.first?.id == item.id, "Receipt arrives before animationCompleted")
        } else if departure == .rejectedSwipe {
            #expect(store.triage.cardStack.departure == nil)
        }
        let receipt = AttachmentFeature.Action.uploaded(attachment.id, .success("stored-proof"))
        let action: TriageCardFeature.Action = switch surface {
        case .question: .question(.attachmentPicker(receipt))
        case .report: .document(.attachmentPicker(receipt))
        case .diff: .pullRequest(.diff(.attachmentPicker(receipt)))
        case .attachedQuestion: .pullRequest(.attachedQuestion(.element(id: 1, action: .question(.attachmentPicker(receipt)))))
        }
        await store.send(.triage(.cards(.element(id: item.id, action: action)))).finish()
        await store.send(.triage(.cards(.element(id: item.id, action: action)))).finish()
        #expect(await sent.count == ((departure == .none || departure == .rejectedSwipe) ? 1 : 0))
        let retained = try #require(store.triage.cards[id: item.id])
        let retainedPicker: AttachmentFeature.State?
        let retainedDraft: String?
        switch surface {
        case .question: retainedPicker = retained.question?.attachmentPicker; retainedDraft = retained.question?.draft.text
        case .report: retainedPicker = retained.document?.attachmentPicker; retainedDraft = retained.document?.draft.general
        case .diff: retainedPicker = retained.pullRequest?.diff.attachmentPicker; retainedDraft = retained.pullRequest?.diff.note
        case .attachedQuestion:
            retainedPicker = retained.pullRequest?.questionFeatures[id: 1]?.question.attachmentPicker
            retainedDraft = retained.pullRequest?.questionFeatures[id: 1]?.question.draft.text
        }
        #expect(retainedDraft == "Keep this draft")
        #expect(retainedPicker?.storedIDs == ["stored-proof"])
        #expect(retainedPicker?.entries.first?.draft == attachment)
    }

    enum Navigation: CaseIterable { case question, report, node, task, request, schedules, cleared, inboxChanged, unchanged }

    @Test(arguments: Navigation.allCases)
    func changingSelectionCancelsPresentedPRBeforeNavigationEffects(navigation: Navigation) async throws {
        let openReport = navigation == .report
        let decoder = JSONDecoder()
        let question = try decoder.decode(Question.self, from: Data(#"{"id":1,"sessionId":"s","title":"Proceed?","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","askedAt":1}"#.utf8))
        let report = try decoder.decode(Report.self, from: Data(#"{"id":99,"sessionId":"s","title":"Report","body":"Body","links":[],"attachments":[],"kind":"document","createdAt":100}"#.utf8))
        let pr = try decoder.decode(PullRequest.self, from: Data(#"{"id":5,"sessionId":"s","url":"https://example.com/p/5","repo":"a/b","number":5,"title":"PR","state":"open","openedAt":1}"#.utf8))
        let attachment = AttachmentDraft(id: UUID(uuidString: "00000000-0000-0000-0000-000000000093")!, name: "proof.txt", data: Data("proof".utf8))
        var picker = AttachmentFeature.State()
        picker.entries = [.init(draft: attachment, status: .uploading)]
        var detail = ServicePullRequestFeature.State(pr: pr, questions: [question])
        detail.diff.note = "Keep review"
        detail.diff.files = []
        detail.diff.loadedDiffText = ""
        detail.diff.loadedHeadSHA = "head"
        detail.diff.attachmentPicker = picker
        detail.diff.pendingSubmission = .awaitingReceipts
        detail.diff.pendingReviewKind = .ask
        detail.questionFeatures[id: 1]?.question.draft.text = "Keep answer"
        detail.questionFeatures[id: 1]?.question.attachmentPicker = picker
        detail.questionFeatures[id: 1]?.question.pendingSubmission = .awaitingReceipts
        detail.questionFeatures[id: 1]?.question.pendingAttachedAnswer = detail.questionFeatures[id: 1]?.question.draft
        var initial = ServiceRootFeature.State()
        initial.pullRequest = detail
        initial.macPresentation.selection = .item("p5")
        initial.macPresentation.inboxIDs = ["p5", "q1", "r99"]
        initial.macPresentation.didAutoSelect = true
        initial.triage.triage.items = [.question(question), .report(report)]
        let sent = SubmissionEvidence()
        let store = Store(initialState: initial) { ServiceRootFeature() } withDependencies: {
            $0.questionWorkService = QuestionWorkService(submit: { _ in await sent.record(); return .obsolete }, document: { _ in "" })
            $0.pullRequestOperations = PullRequestOperations(diff: { _ in PRDiffSnapshot(diff: "", headSHA: "head") },
                merge: { _, _ in .obsolete }, resolveConflicts: { _ in .obsolete }, setAside: { _ in .obsolete },
                review: { _, _, _, _, _ in await sent.record(); return .obsolete })
        }
        let action: ServiceRootFeature.Action = switch navigation {
        case .question, .report: .focusInbox(openReport ? "r99" : "q1")
        case .node: .macPresentation(.select(.node("node")))
        case .task: .macPresentation(.select(.task("task")))
        case .request: .macPresentation(.select(.request("request")))
        case .schedules: .macPresentation(.select(.schedules))
        case .cleared: .macPresentation(.select(nil))
        case .inboxChanged: .macPresentation(.inboxChanged(["q1", "r99"]))
        case .unchanged: .macPresentation(.select(.item("p5")))
        }
        let transition = store.send(action)
        // Inspect before awaiting the queued focus/open effects or a SwiftUI callback.
        #expect((store.pullRequest?.diff.pendingSubmission != nil) == (navigation == .unchanged))
        #expect((store.pullRequest?.questionFeatures[id: 1]?.question.pendingAttachedAnswer != nil) == (navigation == .unchanged))
        let receipt = AttachmentFeature.Action.uploaded(attachment.id, .success("stored-proof"))
        await store.send(.pullRequest(.presented(.diff(.attachmentPicker(receipt))))).finish()
        await store.send(.pullRequest(.presented(.attachedQuestion(.element(id: 1, action: .question(.attachmentPicker(receipt))))))).finish()
        await transition.finish()
        #expect(await sent.count == (navigation == .unchanged ? 2 : 0))
        #expect(store.pullRequest?.diff.note == "Keep review")
        #expect(store.pullRequest?.diff.attachmentPicker.storedIDs == ["stored-proof"])
        #expect(store.pullRequest?.questionFeatures[id: 1]?.question.draft.text == "Keep answer")
        #expect(store.pullRequest?.questionFeatures[id: 1]?.question.attachmentPicker.storedIDs == ["stored-proof"])
        if navigation == .question || navigation == .report {
            #expect(store.triage.document?.subject.id == (openReport ? "r99" : "q1"))
        }
        if navigation == .inboxChanged { #expect(store.macPresentation.selection == .item("q1")) }
    }

    @Test func closingRootDoesNotCancelAlreadyDispatchedCommandOrLoseReconciliation() async throws {
        let question = try JSONDecoder().decode(Question.self, from: Data(#"{"id":1,"sessionId":"s","title":"Proceed?","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","askedAt":1}"#.utf8))
        let command = UUID(uuidString: "00000000-0000-0000-0000-000000000092")!
        var card = TriageCardFeature.State(item: .question(question), identity: nil)
        card.question?.draft.text = "Already sent"
        var initial = ServiceRootFeature.State()
        initial.triage.triage.items = [.question(question)]
        initial.triage.cards = [card]
        let final = FinalCommandGate()
        let store = Store(initialState: initial) { ServiceRootFeature() } withDependencies: {
            $0.questionWorkService = QuestionWorkService(submit: { _ in
                await final.waitForRelease()
                #expect(!Task.isCancelled, "Leaving the scene must not cancel an already dispatched command")
                return .uncertainCommand(command)
            }, document: { _ in "" })
        }
        let submission = store.send(.triage(.cards(.element(id: "q1", action: .question(.answerTapped)))))
        await final.waitUntilStarted()
        #expect(store.triage.cards[id: "q1"]?.question?.isSubmitting == true)
        await store.send(.disappeared).finish()
        #expect(store.triage.cards[id: "q1"]?.question?.isSubmitting == true)
        await final.release()
        await submission.finish()
        let result = try #require(store.triage.cards[id: "q1"]?.question)
        #expect(result.needsReconciliation)
        #expect(result.uncertainCommandID == command)
        #expect(result.draft.text == "Already sent")
        #expect(!result.isSubmitting)
    }

}

private actor SubmissionEvidence {
    private(set) var count = 0
    func record() { count += 1 }
}

private actor FinalCommandGate {
    private var started = false
    private var released = false
    private var startWaiter: CheckedContinuation<Void, Never>?
    private var releaseWaiter: CheckedContinuation<Void, Never>?
    func waitForRelease() async {
        started = true
        startWaiter?.resume(); startWaiter = nil
        if !released { await withCheckedContinuation { releaseWaiter = $0 } }
    }
    func waitUntilStarted() async {
        if !started { await withCheckedContinuation { startWaiter = $0 } }
    }
    func release() {
        released = true
        releaseWaiter?.resume(); releaseWaiter = nil
    }
}
