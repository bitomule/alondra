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
struct CardRejectionIntegrationTests {
    enum Route: CaseIterable { case feedback, comments, approve, ask, changes, merge, conflicts }
    private let file = AttachmentDraft(id: UUID(uuidString: "00000000-0000-0000-0000-000000000082")!, name: "proof.txt", data: Data("proof".utf8))
    private static let receipt = DomainV2.AttachmentRef(id: "receipt", name: "proof.txt", contentDigest: Data("proof".utf8).sha256Hex,
                                                        contentReference: "ref", bytes: 5)

    private func draft(_ route: Route) -> String { route == .approve ? "" : "Keep this draft" }

    private func initial(_ route: Route) throws -> (ServiceRootFeature.State, TriageCardFeature.State) {
        var fixture = V2Fixture()
        let root: DomainV2.InboxItem
        switch route {
        case .feedback, .comments, .approve: root = fixture.documentReview("a")
        default:
            root = fixture.pullRequest(5)
            let mergeable = route == .conflicts ? "CONFLICTING" : "MERGEABLE"
            fixture.runtime = V2Fixture.runtime(#""nodes":[],"pull_requests":[{"pr_id":"pr-5","observation":{"id":5,"session_id":"s","url":"https://example.com/p/5","repo":"a/b","number":5,"title":"PR","state":"open","opened_at":1,"mergeable":"\#(mergeable)","merge_methods":["merge"]}}]"#)
        }
        var card = TriageCardFeature.State(item: root, workspace: fixture.workspace, identity: V2Fixture.workIdentity)
        let entries = [AttachmentFeature.Entry(draft: file, status: .uploaded(Self.receipt))]
        if card.document != nil {
            card.document?.sourceText = "Body"
            card.document?.draft.general = draft(route)
            if route == .feedback { card.document?.attachmentPicker.entries = entries }
        } else {
            // An already-loaded diff is a valid review snapshot; the real review use case reads
            // it again through the transport below before sending.
            card.pullRequest?.diff.files = []
            card.pullRequest?.diff.loadedDiffText = ""
            card.pullRequest?.diff.loadedHeadSHA = V2Fixture.head
            card.pullRequest?.diff.note = draft(route)
            card.pullRequest?.diff.attachmentPicker.entries = entries
        }
        var state = ServiceRootFeature.State()
        state.triage.cards = [card]
        return (state, card)
    }

    private func action(_ route: Route) -> TriageCardFeature.Action {
        switch route {
        case .feedback, .changes: .send
        case .comments: .document(.reviewTapped(.comments))
        case .approve: .document(.reviewTapped(.approved))
        case .ask: .ask
        case .merge: .pullRequest(.pr(.mergeMethodConfirmed(.merge)))
        case .conflicts: .pullRequest(.pr(.resolveConflictsTapped))
        }
    }

    private func expectedIntent(_ route: Route, _ intent: DomainIntent?) {
        switch (route, intent) {
        case (.feedback, .submitFeedback("delivery-a", let body, let attachments)?):
            #expect(body.contains("Keep this draft") && attachments == [Self.receipt])
        case (.comments, .submitFeedback("delivery-a", let body, _)?): #expect(body.contains("Keep this draft"))
        case (.approve, .acceptDelivery("delivery-a")?): break
        case (.ask, .askAboutPullRequest("pr-5", V2Fixture.head, let body, let attachments)?):
            #expect(body.contains("Keep this draft") && attachments == [Self.receipt])
        case (.changes, .submitFeedback("delivery-5", let body, let attachments)?):
            #expect(body.contains("Keep this draft") && attachments == [Self.receipt])
        case (.merge, .authorizeMerge("delivery-5", .merge)?), (.conflicts, .requestConflictResolution("pr-5", V2Fixture.head)?): break
        default: Issue.record("Wrong command for \(route): \(String(describing: intent))")
        }
    }

    private func finishing(_ observed: AsyncStream<Void>.Continuation, _ notices: LockIsolated<Int>) -> Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> {
        Reduce { state, action in
            switch action {
            case .triage(.cards(.element(_, .document(.submissionFinished)))),
                 .triage(.cards(.element(_, .pullRequest(.diff(.reviewFinished))))),
                 .triage(.cards(.element(_, .pullRequest(.pr(.mutationFinished))))),
                 .triage(.documents(.element(_, .submissionFinished))),
                 .pullRequests(.element(_, .diff(.reviewFinished))),
                 .pullRequests(.element(_, .pr(.mutationFinished))):
                if state.notice != nil { notices.withValue { $0 += 1 } }
                observed.yield(())
            default: break
            }
            return .none
        }
    }

    @Test(.timeLimit(.minutes(1)), arguments: Route.allCases)
    func terminalRejectionUsesOneToastAndRetainsCardDraft(route: Route) async throws {
        let (initial, card) = try initial(route)
        let id = card.id
        let clock = TestClock()
        let log = CommandLog()
        let observed = AsyncStream<Void>.makeStream()
        let notices = LockIsolated(0)
        let store = Store(initialState: initial) {
            CombineReducers { ServiceRootFeature(); finishing(observed.continuation, notices) }
        } withDependencies: {
            $0.continuousClock = clock
            $0.domainWorkspace = .scripted(log, documents: ["rev-a": "Body"]) { _ in V2Fixture.rejected() }
        }
        let operation = store.send(.triage(.cards(.element(id: id, action: action(route)))))
        var iterator = observed.stream.makeAsyncIterator()
        _ = await iterator.next()
        #expect(log.count == 1)
        expectedIntent(route, log.intents.first)
        #expect(notices.value == 1)
        #expect(store.notice?.event == .failure(couldNotSendMessage("Rejected")))
        let retained = try #require(store.triage.cards[id: id])
        #expect(retained.notice == nil)
        #expect(retained.reply == draft(route))
        #expect(retained.stamp == nil)
        if route == .feedback {
            #expect(retained.document?.attachmentPicker.stored == [Self.receipt])
            #expect(retained.document?.attachmentPicker.entries.first?.draft == file)
        } else if retained.pullRequest != nil {
            #expect(retained.pullRequest?.diff.attachmentPicker.entries.first?.draft == file)
            #expect(retained.pullRequest?.diff.attachmentPicker.stored == [Self.receipt])
        }
        await clock.advance(by: .seconds(6))
        await operation.finish()
        #expect(store.notice == nil)
        #expect(store.triage.cards[id: id]?.notice == nil)
        // A delayed completion after the command finished is not a new error, even if it reuses
        // the current epoch. Neither stale nor duplicate results toast.
        for epoch in [UInt64.max, 0] {
            let stale: TriageCardFeature.Action = switch route {
            case .feedback, .comments, .approve: .document(.submissionFinished(epoch, V2Fixture.rejected("Late")))
            case .ask, .changes: .pullRequest(.diff(.reviewFinished(.rejected("Late"), epoch)))
            default: .pullRequest(.pr(.mutationFinished(.rejected("Late"), epoch)))
            }
            await store.send(.triage(.cards(.element(id: id, action: stale)))).finish()
            #expect(store.notice == nil)
        }
        // A later local validation shows on the card again, without inheriting the terminal
        // failure's toast or dispatching another command.
        if route == .feedback {
            await store.send(.triage(.cards(.element(id: id, action: .document(.reviewTapped(.approved)))))).finish()
            #expect(store.triage.cards[id: id]?.notice == "Send your comments or clear them before accepting.")
        } else if route == .ask || route == .changes {
            await store.send(.triage(.cards(.element(id: id, action: .replyChanged(""))))).finish()
            await store.send(.triage(.cards(.element(id: id, action: .ask)))).finish()
            #expect(store.triage.cards[id: id]?.notice == "Add a note or line comment")
        }
        #expect(log.count == 1)
        #expect(store.notice == nil)
        observed.continuation.finish()
    }

    @Test(.timeLimit(.minutes(1)), arguments: Route.allCases)
    func detailRejectionUsesOneToastAndRetainsDraft(route: Route) async throws {
        var (initial, card) = try initial(route)
        initial.triage.cards = []
        initial.triage.document = card.document
        initial.pullRequest = card.pullRequest
        let documentID = card.document?.id ?? card.id
        func detailAction(_ action: TriageCardFeature.Action) -> ServiceRootFeature.Action {
            switch action {
            case .document(let action): return .triage(.documents(.element(id: documentID, action: action)))
            case .pullRequest(let action): return .pullRequests(.element(id: "pr-5", action: action))
            default: preconditionFailure("Detail test must use its real leaf action")
            }
        }
        let submit: ServiceRootFeature.Action = switch route {
        case .feedback: .triage(.documents(.element(id: documentID, action: .sendFeedbackTapped(author: ""))))
        case .changes: .pullRequests(.element(id: "pr-5", action: .diff(.requestChangesTapped)))
        case .ask: .pullRequests(.element(id: "pr-5", action: .diff(.askTapped)))
        default: detailAction(action(route))
        }
        let clock = TestClock()
        let log = CommandLog()
        let observed = AsyncStream<Void>.makeStream()
        let notices = LockIsolated(0)
        let store = Store(initialState: initial) {
            CombineReducers { ServiceRootFeature(); finishing(observed.continuation, notices) }
        } withDependencies: {
            $0.continuousClock = clock
            $0.domainWorkspace = .scripted(log, documents: ["rev-a": "Body"]) { _ in V2Fixture.rejected() }
        }
        let operation = store.send(submit)
        var iterator = observed.stream.makeAsyncIterator()
        _ = await iterator.next()
        #expect(log.count == 1)
        expectedIntent(route, log.intents.first)
        #expect(notices.value == 1)
        #expect(store.notice?.event == .failure(couldNotSendMessage("Rejected")))
        if let document = store.triage.document {
            #expect(document.noticeIsSubmissionFailure)
            #expect(document.draft.general == draft(route))
            #expect(!document.completed)
            if route == .feedback {
                #expect(document.attachmentPicker.stored == [Self.receipt])
                #expect(document.attachmentPicker.entries.first?.draft == file)
            }
        } else {
            let pr = try #require(store.pullRequest)
            #expect(pr.diff.note == draft(route))
            #expect(pr.diff.attachmentPicker.stored == [Self.receipt])
            #expect(!pr.pr.shouldDismiss && !pr.diff.shouldDismiss)
            if route == .ask || route == .changes { #expect(pr.diff.validationErrorIsSubmissionFailure) }
        }
        await clock.advance(by: .seconds(6))
        await operation.finish()
        #expect(store.notice == nil)
        for epoch in [UInt64.max, 0] {
            let stale: TriageCardFeature.Action = switch route {
            case .feedback, .comments, .approve: .document(.submissionFinished(epoch, V2Fixture.rejected("Late")))
            case .ask, .changes: .pullRequest(.diff(.reviewFinished(.rejected("Late"), epoch)))
            default: .pullRequest(.pr(.mutationFinished(.rejected("Late"), epoch)))
            }
            await store.send(detailAction(stale)).finish()
            #expect(store.notice == nil)
        }
        #expect(log.count == 1)
        observed.continuation.finish()
    }

    @Test(.timeLimit(.minutes(1)), arguments: [Route.feedback, .ask, .merge])
    func anUnknownOutcomeKeepsTheDraftAndTryingAgainIsTheSameCommand(route: Route) async throws {
        let (initial, card) = try initial(route)
        let id = card.id
        let clock = TestClock()
        let log = CommandLog()
        let results = LockIsolated<[DomainCommandResult]>([.uncertain, V2Fixture.rejected()])
        let observed = AsyncStream<Void>.makeStream()
        let notices = LockIsolated(0)
        let store = Store(initialState: initial) {
            CombineReducers { ServiceRootFeature(); finishing(observed.continuation, notices) }
        } withDependencies: {
            $0.continuousClock = clock
            $0.uuid = .incrementing
            $0.domainWorkspace = .scripted(log, documents: ["rev-a": "Body"]) { _ in results.withValue { $0.removeFirst() } }
        }
        func send(_ action: TriageCardFeature.Action) -> StoreTask {
            store.send(.triage(.cards(.element(id: id, action: action))))
        }
        var iterator = observed.stream.makeAsyncIterator()
        await send(action(route)).finish()
        _ = await iterator.next()
        #expect(log.count == 1)
        #expect(store.notice == nil)
        let unknown = try #require(store.triage.cards[id: id])
        switch route {
        case .feedback:
            #expect(unknown.notice == "We couldn’t confirm the result. Your draft is kept; sending it again won’t apply it twice.")
            #expect(unknown.document?.needsReconciliation == true)
        case .ask: #expect(unknown.pullRequest?.diff.mutationOutcome == .unknown)
        default: #expect(unknown.pullRequest?.pr.mutationOutcome == .unknown)
        }
        #expect(unknown.reply == "Keep this draft")
        let retry = send(action(route))
        _ = await iterator.next()
        #expect(log.count == 2)
        #expect(Set(log.commandIDs).count == 1, "the retry is the same command, so the server applies it once")
        #expect(store.notice?.event == .failure(couldNotSendMessage("Rejected")))
        #expect(store.triage.cards[id: id]?.reply == "Keep this draft")
        await clock.advance(by: .seconds(6))
        await retry.finish()
        observed.continuation.finish()
    }
}
