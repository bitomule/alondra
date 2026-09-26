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
    enum Route: CaseIterable { case feedback, comments, approve, ask, changes, merge, conflicts, setAside }
    private let command = UUID(uuidString: "00000000-0000-0000-0000-000000000081")!
    private let file = AttachmentDraft(id: UUID(uuidString: "00000000-0000-0000-0000-000000000082")!, name: "proof.txt", data: Data("proof".utf8))

    private func initial(_ route: Route) throws -> (ServiceRootFeature.State, String) {
        let decoder = JSONDecoder()
        let identity = QuestionWorkIdentity(server: URL(string: "https://service.example")!, deviceID: "phone", credentialRevision: "one", generation: 1)
        var card: TriageCardFeature.State
        switch route {
        case .feedback, .comments, .approve:
            let report = try decoder.decode(Report.self, from: Data(#"{"id":7,"sessionId":"s","title":"Report","body":"Body","links":[{"url":"https://claude.ai/artifact/review-1"}],"attachments":[],"kind":"document","createdAt":1}"#.utf8))
            card = TriageCardFeature.State(item: .report(report), identity: identity)
            card.document?.sourceText = "Body"
            card.document?.draft.general = "Keep this draft"
            if route == .feedback {
                card.document?.attachmentPicker.entries = [.init(draft: file, status: .uploaded("receipt"))]
                card.document?.draft.attachmentIDs = ["receipt"]
            }
        default:
            let conflict = route == .conflicts ? "CONFLICTING" : "MERGEABLE"
            let pr = try decoder.decode(PullRequest.self, from: Data("""
            {"id":5,"sessionId":"s","url":"https://example.com/p/5","repo":"a/b","number":5,"title":"PR","state":"open","openedAt":1,"mergeable":"\(conflict)","mergeMethods":["merge"]}
            """.utf8))
            card = TriageCardFeature.State(item: .pullRequest(pr), identity: identity)
            // An already-loaded empty diff is a valid review snapshot; the real
            // SubmitPRReviewUseCase re-fetches it through the transport below.
            card.pullRequest?.diff.files = []
            card.pullRequest?.diff.loadedDiffText = ""
            card.pullRequest?.diff.loadedHeadSHA = "head"
            card.pullRequest?.diff.note = "Keep this draft"
            card.pullRequest?.diff.attachmentPicker.entries = [.init(draft: file, status: .uploaded("receipt"))]
            card.pullRequest?.diff.attachments = [.init(id: file.id, name: file.name, data: file.data, storedID: "receipt")]
        }
        var root = ServiceRootFeature.State()
        root.triage.cards = [card]
        return (root, card.id)
    }

    private func action(_ route: Route) -> TriageCardFeature.Action {
        switch route {
        case .feedback, .changes: .send
        case .comments: .document(.reviewTapped(.comments))
        case .approve: .document(.reviewTapped(.approved))
        case .ask: .ask
        case .merge: .pullRequest(.pr(.mergeMethodConfirmed("merge")))
        case .conflicts: .pullRequest(.pr(.resolveConflictsTapped))
        case .setAside: .pullRequest(.pr(.setAsideTapped))
        }
    }

    @Test(.timeLimit(.minutes(1)), arguments: Route.allCases)
    func terminalRejectionUsesOneToastAndRetainsCardDraft(route: Route) async throws {
        let (initial, id) = try initial(route)
        let clock = TestClock()
        let finals = LockIsolated<[String]>([])
        let observed = AsyncStream<Void>.makeStream()
        let notices = LockIsolated(0)
        let file = file
        let store = Store(initialState: initial) {
            CombineReducers {
                ServiceRootFeature()
                Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> { state, action in
                    switch action {
                    case .triage(.cards(.element(_, .document(.submissionFinished)))),
                         .triage(.cards(.element(_, .document(.reportFeedbackFinished)))),
                         .triage(.cards(.element(_, .pullRequest(.diff(.reviewFinished))))),
                         .triage(.cards(.element(_, .pullRequest(.pr(.mutationFinished))))):
                        if state.notice != nil { notices.withValue { $0 += 1 } }
                        observed.continuation.yield(())
                    default: break
                    }
                    return .none
                }
            }
        } withDependencies: {
            $0.continuousClock = clock
            $0.questionWorkService = .init(submit: { operation in
                finals.withValue { $0.append("review") }
                #expect(operation == .reviewReport(id: 7, verdict: route == .approve ? .approved : .comments))
                return route == .approve ? .forbidden("Rejected") : .conflict("Rejected")
            }, submitReportFeedback: { request, _ in
                finals.withValue { $0.append("feedback") }
                #expect(request.reportID == 7 && request.attachmentIDs == ["receipt"])
                #expect(request.text.contains("Keep this draft"))
                return .failed("Rejected")
            }, document: { _ in "Body" })
            $0.pullRequestOperations = .init(diff: { _ in .init(diff: "", headSHA: "head") }, merge: { _, _ in
                finals.withValue { $0.append("merge") }; return .rejected("Rejected")
            }, resolveConflicts: { _ in
                finals.withValue { $0.append("conflicts") }; return .rejected("Rejected")
            }, setAside: { _ in
                finals.withValue { $0.append("setAside") }; return .rejected("Rejected")
            }, review: { id, text, attachments, kind, head in
                finals.withValue { $0.append("diff") }
                #expect(id == 5 && head == "head" && text.contains("Keep this draft"))
                #expect(attachments == [.init(id: file.id, name: file.name, data: file.data, storedID: "receipt")])
                switch (route, kind) { case (.ask, .ask), (.changes, .requestChanges): break; default: Issue.record("Wrong review operation") }
                return .rejected("Rejected")
            })
        }
        let operation = store.send(.triage(.cards(.element(id: id, action: action(route)))))
        var iterator = observed.stream.makeAsyncIterator()
        _ = await iterator.next()
        #expect(finals.value.count == 1)
        #expect(notices.value == 1)
        #expect(store.notice?.event == .failure(couldNotSendMessage("Rejected")))
        let card = try #require(store.triage.cards[id: id])
        #expect(card.notice == nil)
        #expect(card.reply == "Keep this draft")
        #expect(card.stamp == nil)
        if route == .feedback {
            #expect(card.document?.draft.attachmentIDs == ["receipt"])
            #expect(card.document?.attachmentPicker.entries.first?.draft == file)
        } else if card.pullRequest != nil {
            #expect(card.pullRequest?.diff.attachmentPicker.entries.first?.draft == file)
            #expect(card.pullRequest?.diff.attachmentPicker.storedIDs == ["receipt"])
        }
        await clock.advance(by: .seconds(6))
        await operation.finish()
        #expect(store.notice == nil)
        #expect(store.triage.cards[id: id]?.notice == nil)
        // A delayed completion after the accepted command finished is not a new error,
        // even if it reuses the current epoch. Neither stale nor duplicate results toast.
        for epoch in [UInt64.max, 0] {
            let stale: TriageCardFeature.Action
            switch route {
            case .feedback: stale = .document(.reportFeedbackFinished(epoch, .failed("Late")))
            case .comments, .approve: stale = .document(.submissionFinished(epoch, .reviewReport(id: 7, verdict: .approved), .failed("Late")))
            case .ask, .changes: stale = .pullRequest(.diff(.reviewFinished(.rejected("Late"), epoch)))
            default: stale = .pullRequest(.pr(.mutationFinished(.rejected("Late"), epoch)))
            }
            await store.send(.triage(.cards(.element(id: id, action: stale)))).finish()
            #expect(store.notice == nil)
        }
        // A subsequent local validation must be visible again, not inherit the
        // terminal-failure classification or dispatch another command.
        if route == .feedback {
            await store.send(.triage(.cards(.element(id: id, action: .document(.reviewTapped(.approved)))))).finish()
            #expect(store.triage.cards[id: id]?.notice == "Send feedback to include your attachments before approving the artifact.")
        } else if route == .ask || route == .changes {
            await store.send(.triage(.cards(.element(id: id, action: .replyChanged(""))))).finish()
            await store.send(.triage(.cards(.element(id: id, action: .ask)))).finish()
            #expect(store.triage.cards[id: id]?.notice == "Add a note or line comment before sending attachments")
        }
        #expect(finals.value.count == 1)
        #expect(store.notice == nil)
        observed.continuation.finish()
    }

    @Test(.timeLimit(.minutes(1)), arguments: Route.allCases)
    func detailRejectionUsesOneToastAndRetainsDraft(route: Route) async throws {
        var (initial, id) = try initial(route)
        let source = try #require(initial.triage.cards[id: id])
        initial.triage.cards = []
        initial.triage.document = source.document
        initial.pullRequest = source.pullRequest
        func detailAction(_ action: TriageCardFeature.Action) -> ServiceRootFeature.Action {
            switch action {
            case .document(let action): return .triage(.document(.presented(action)))
            case .pullRequest(let action): return .pullRequest(.presented(action))
            default: preconditionFailure("Detail test must use its real leaf action")
            }
        }
        let submit: ServiceRootFeature.Action
        switch route {
        case .feedback: submit = .triage(.document(.presented(.sendFeedbackTapped(author: "Reviewer"))))
        case .changes: submit = .pullRequest(.presented(.diff(.requestChangesTapped)))
        case .ask: submit = .pullRequest(.presented(.diff(.askTapped)))
        default: submit = detailAction(action(route))
        }
        let clock = TestClock()
        let finals = LockIsolated<[String]>([])
        let observed = AsyncStream<Void>.makeStream()
        let notices = LockIsolated(0)
        let file = file
        let store = Store(initialState: initial) {
            CombineReducers {
                ServiceRootFeature()
                Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> { state, action in
                    switch action {
                    case .triage(.document(.presented(.submissionFinished))),
                         .triage(.document(.presented(.reportFeedbackFinished))),
                         .pullRequest(.presented(.diff(.reviewFinished))),
                         .pullRequest(.presented(.pr(.mutationFinished))):
                        if state.notice != nil { notices.withValue { $0 += 1 } }
                        observed.continuation.yield(())
                    default: break
                    }
                    return .none
                }
            }
        } withDependencies: {
            $0.continuousClock = clock
            $0.questionWorkService = .init(submit: { operation in
                finals.withValue { $0.append("review") }
                #expect(operation == .reviewReport(id: 7, verdict: route == .approve ? .approved : .comments))
                return route == .approve ? .forbidden("Rejected") : .conflict("Rejected")
            }, submitReportFeedback: { request, _ in
                finals.withValue { $0.append("feedback") }
                #expect(request.reportID == 7 && request.attachmentIDs == ["receipt"])
                #expect(request.text.contains("Keep this draft"))
                return .failed("Rejected")
            }, document: { _ in "Body" })
            $0.pullRequestOperations = .init(diff: { _ in .init(diff: "", headSHA: "head") }, merge: { _, _ in
                finals.withValue { $0.append("merge") }; return .rejected("Rejected")
            }, resolveConflicts: { _ in
                finals.withValue { $0.append("conflicts") }; return .rejected("Rejected")
            }, setAside: { _ in
                finals.withValue { $0.append("setAside") }; return .rejected("Rejected")
            }, review: { id, text, attachments, kind, head in
                finals.withValue { $0.append("diff") }
                #expect(id == 5 && head == "head" && text.contains("Keep this draft"))
                #expect(attachments == [.init(id: file.id, name: file.name, data: file.data, storedID: "receipt")])
                switch (route, kind) { case (.ask, .ask), (.changes, .requestChanges): break; default: Issue.record("Wrong review operation") }
                return .rejected("Rejected")
            })
        }
        let operation = store.send(submit)
        var iterator = observed.stream.makeAsyncIterator()
        _ = await iterator.next()
        #expect(finals.value.count == 1)
        #expect(notices.value == 1)
        #expect(store.notice?.event == .failure(couldNotSendMessage("Rejected")))
        if let document = store.triage.document {
            #expect(document.noticeIsSubmissionFailure)
            #expect(document.draft.general == "Keep this draft")
            #expect(!document.completed)
            if route == .feedback {
                #expect(document.draft.attachmentIDs == ["receipt"])
                #expect(document.attachmentPicker.entries.first?.draft == file)
            }
        } else {
            let pr = try #require(store.pullRequest)
            #expect(pr.diff.note == "Keep this draft")
            #expect(pr.diff.attachmentPicker.storedIDs == ["receipt"])
            #expect(!pr.pr.shouldDismiss && !pr.diff.shouldDismiss)
            if route == .ask || route == .changes { #expect(pr.diff.validationErrorIsSubmissionFailure) }
        }
        await clock.advance(by: .seconds(6))
        await operation.finish()
        #expect(store.notice == nil)
        // A delayed completion after the accepted command finished is not a new error,
        // even if it reuses the current epoch. Neither stale nor duplicate results toast.
        for epoch in [UInt64.max, 0] {
            let stale: TriageCardFeature.Action
            switch route {
            case .feedback: stale = .document(.reportFeedbackFinished(epoch, .failed("Late")))
            case .comments, .approve: stale = .document(.submissionFinished(epoch, .reviewReport(id: 7, verdict: .approved), .failed("Late")))
            case .ask, .changes: stale = .pullRequest(.diff(.reviewFinished(.rejected("Late"), epoch)))
            default: stale = .pullRequest(.pr(.mutationFinished(.rejected("Late"), epoch)))
            }
            await store.send(detailAction(stale)).finish()
            #expect(store.notice == nil)
        }
        #expect(finals.value.count == 1)
        observed.continuation.finish()
    }

    @Test(.timeLimit(.minutes(1)), arguments: [Route.feedback, .ask, .merge])
    func uncertainCommandStaysBlockedAndOnlyMatchingRecoveryShowsRejection(route: Route) async throws {
        let (initial, id) = try initial(route)
        let command = command
        let clock = TestClock()
        let dispatches = LockIsolated(0)
        let recovered = AsyncStream<Void>.makeStream()
        let store = Store(initialState: initial) {
            CombineReducers {
                ServiceRootFeature()
                Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> { state, action in
                    switch action {
                    case .triage(.cards(.element(_, .document(.commandResolved)))),
                         .triage(.cards(.element(_, .pullRequest(.diff(.reviewFinished))))),
                         .triage(.cards(.element(_, .pullRequest(.pr(.mutationFinished))))):
                        if state.notice != nil { recovered.continuation.yield(()) }
                    default: break
                    }
                    return .none
                }
            }
        } withDependencies: {
            $0.continuousClock = clock
            $0.questionWorkService = .init(submit: { _ in Issue.record("Unexpected review"); return .obsolete },
                submitReportFeedback: { _, _ in dispatches.withValue { $0 += 1 }; return .uncertainCommand(command) }, document: { _ in "Body" })
            $0.pullRequestOperations = .init(diff: { _ in .init(diff: "", headSHA: "head") }, merge: { _, _ in
                dispatches.withValue { $0 += 1 }; return .unknown(command)
            }, resolveConflicts: { _ in Issue.record("Unexpected conflict command"); return .obsolete },
            setAside: { _ in Issue.record("Unexpected set-aside"); return .obsolete }, review: { _, _, _, _, _ in
                dispatches.withValue { $0 += 1 }; return .unknown(command)
            })
        }
        func send(_ action: TriageCardFeature.Action) -> StoreTask {
            store.send(.triage(.cards(.element(id: id, action: action))))
        }
        await send(action(route)).finish()
        #expect(dispatches.value == 1)
        #expect(store.notice == nil)
        switch route {
        case .feedback:
            #expect(store.triage.cards[id: id]?.notice == "The result is uncertain. Checking its status…")
            #expect(store.triage.cards[id: id]?.document?.needsReconciliation == true)
        case .ask: #expect(store.triage.cards[id: id]?.pullRequest?.diff.canReview == false)
        case .merge: #expect(store.triage.cards[id: id]?.pullRequest?.pr.canAct == false)
        default: Issue.record("Unsupported recovery route")
        }
        await send(action(route)).finish()
        #expect(dispatches.value == 1)
        func resolution(_ uuid: UUID) -> TriageCardFeature.Action {
            switch route {
            case .feedback: .document(.commandResolved(uuid, rejection: "Rejected"))
            case .ask: .pullRequest(.diff(.commandResolved(uuid, rejection: "Rejected")))
            default: .pullRequest(.pr(.commandResolved(uuid, rejection: "Rejected")))
            }
        }
        await send(resolution(UUID(uuidString: "00000000-0000-0000-0000-000000000099")!)).finish()
        #expect(store.notice == nil)
        let recovery = send(resolution(command))
        var iterator = recovered.stream.makeAsyncIterator()
        _ = await iterator.next()
        #expect(store.notice?.event == .failure(couldNotSendMessage("Rejected")))
        #expect(store.triage.cards[id: id]?.notice == nil)
        #expect(store.triage.cards[id: id]?.reply == "Keep this draft")
        #expect(dispatches.value == 1)
        await clock.advance(by: .seconds(6))
        await recovery.finish()
        recovered.continuation.finish()
    }

}
