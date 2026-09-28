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
struct PullRequestOwnerIntegrationTests {
    private func initial() throws -> ServiceRootFeature.State {
        func pr(_ id: Int) throws -> PullRequest {
            try JSONDecoder().decode(PullRequest.self, from: Data("""
            {"id":\(id),"sessionId":"s","url":"https://example.com/p/\(id)","repo":"a/b","number":\(id),"title":"PR\(id)","state":"open","openedAt":1}
            """.utf8))
        }
        var state = ServiceRootFeature.State()
        state.triage.triage.identity = TriageIdentity(server: URL(string: "https://service.example")!, deviceID: "phone", credentialRevision: "one", generation: 1)
        state.triage.triage.items = [.pullRequest(try pr(5)), .pullRequest(try pr(6))]
        return state
    }

    private func diff(_ id: UInt64, _ action: DiffReviewFeature.Action) -> ServiceRootFeature.Action {
        .pullRequests(.element(id: id, action: .diff(action)))
    }

    @Test(.timeLimit(.minutes(1)))
    func navigationRetainsDraftAndUploadReceiptWithoutSendingAbandonedIntent() async throws {
        let file = AttachmentDraft(name: "proof.txt", data: Data("proof".utf8))
        let started = AsyncStream<Void>.makeStream()
        let receipt = AsyncStream<String>.makeStream()
        let imported = AsyncStream<Void>.makeStream()
        let sent = LockIsolated<[UInt64]>([])
        let initialState = try initial()
        let store = Store(initialState: initialState) {
            CombineReducers {
                ServiceRootFeature()
                Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> { _, action in
                    if case .pullRequests(.element(5, .diff(.attachmentPicker(.importFinished)))) = action {
                        imported.continuation.yield(())
                    }
                    return .none
                }
            }
        } withDependencies: {
            $0.continuousClock = ImmediateClock()
            $0.uuid = .incrementing
            $0.attachmentBudget = AttachmentBudget()
            $0.attachmentImport = .init(file: { _ in file }, photo: { _ in file })
            $0.attachmentUpload = .init(upload: { draft in
                #expect(draft == file)
                started.continuation.yield(())
                var iterator = receipt.stream.makeAsyncIterator()
                return await iterator.next() ?? "Unexpected termination"
            })
            $0.pullRequestOperations = .init(diff: { _ in .init(diff: "", headSHA: "head") },
                merge: { _, _ in .obsolete }, resolveConflicts: { _ in .obsolete }, setAside: { _ in .obsolete },
                review: { id, text, files, _, _ in
                    #expect(text.contains("Keep A"))
                    #expect(files.first?.id == file.id && files.first?.storedID == "receipt")
                    sent.withValue { $0.append(id) }
                    return .rejected("Retain draft")
                })
        }
        await store.send(.focusInbox("p5")).finish()
        await store.send(diff(5, .appeared)).finish()
        await store.send(diff(5, .noteChanged("Keep A"))).finish()
        let upload = store.send(diff(5, .attachmentPicker(.filesSelected([URL(fileURLWithPath: "/fixture/proof.txt")]))))
        var iterator = started.stream.makeAsyncIterator()
        _ = await iterator.next()
        var completedImport = imported.stream.makeAsyncIterator()
        _ = await completedImport.next()
        await store.send(diff(5, .askTapped)).finish()
        #expect(store.pullRequests[id: 5]?.diff.pendingSubmission != nil)
        await store.send(.focusInbox("p6")).finish()
        await store.send(diff(5, .disappeared)).finish()
        #expect(store.pullRequestID == 6)
        #expect(store.pullRequests[id: 5]?.diff.pendingSubmission == nil)
        receipt.continuation.yield("receipt")
        receipt.continuation.finish()
        await upload.finish()
        #expect(sent.value.isEmpty)
        #expect(store.pullRequests[id: 5]?.diff.note == "Keep A")
        #expect(store.pullRequests[id: 5]?.diff.attachmentPicker.storedIDs == ["receipt"])
        await store.send(.pullRequestDismissed(6)).finish()
        #expect(store.pullRequest == nil && store.pullRequests.count == 2)
        await store.send(.focusInbox("p5")).finish()
        #expect(store.pullRequest?.diff.note == "Keep A")
        await store.send(diff(5, .askTapped)).finish()
        #expect(sent.value == [5])
        #expect(store.pullRequest?.diff.note == "Keep A")
        started.continuation.finish()
        imported.continuation.finish()
    }

    @Test(.timeLimit(.minutes(1)))
    func dispatchedReviewStaysOwnedAndReconcilesWhileAnotherPRIsVisible() async throws {
        let started = AsyncStream<Void>.makeStream()
        let result = AsyncStream<PRMutationOutcome>.makeStream()
        let calls = LockIsolated(0)
        let commandID = UUID()
        let scope = CommandScope(server: URL(string: "https://service.example")!, deviceID: "phone", credentialRevision: "one", generation: 1)
        let initialState = try initial()
        let store = Store(initialState: initialState) { ServiceRootFeature() } withDependencies: {
            $0.continuousClock = ImmediateClock()
            $0.pullRequestOperations = .init(diff: { _ in .init(diff: "", headSHA: "head") },
                merge: { _, _ in .obsolete }, resolveConflicts: { _ in .obsolete }, setAside: { _ in .obsolete },
                review: { _, _, _, _, _ in
                    calls.withValue { $0 += 1 }
                    started.continuation.yield(())
                    var iterator = result.stream.makeAsyncIterator()
                    return await iterator.next() ?? .obsolete
                })
        }
        await store.send(.focusInbox("p5")).finish()
        await store.send(diff(5, .appeared)).finish()
        await store.send(diff(5, .noteChanged("A in flight"))).finish()
        let command = store.send(diff(5, .askTapped))
        var iterator = started.stream.makeAsyncIterator()
        _ = await iterator.next()
        await store.send(.pullRequestDismissed(5)).finish()
        await store.send(diff(5, .disappeared)).finish()
        await store.send(.focusInbox("p6")).finish()
        await store.send(.pullRequestDismissed(5)).finish()
        #expect(store.pullRequestID == 6)
        await store.send(diff(6, .noteChanged("B draft"))).finish()
        result.continuation.yield(.unknown(commandID))
        result.continuation.finish()
        await command.finish()
        #expect(store.pullRequests[id: 5]?.diff.mutationOutcome == .unknown(commandID))
        #expect(store.pullRequestID == 6 && store.pullRequest?.diff.note == "B draft")
        await store.send(.focusInbox("p5")).finish()
        await store.send(diff(5, .askTapped)).finish()
        #expect(calls.value == 1)
        await store.send(.focusInbox("p6")).finish()
        await store.send(.commandResolved(.init(command: .init(id: UUID(), scope: scope, kind: .askOnPullRequest(5), itemID: nil), status: 200))).finish()
        #expect(store.pullRequests[id: 5]?.diff.mutationOutcome == .unknown(commandID))
        let resolution = CommandResolution(command: .init(id: commandID, scope: scope, kind: .askOnPullRequest(5), itemID: nil), status: 200)
        await store.send(.commandResolved(resolution)).finish()
        #expect(store.pullRequests[id: 5]?.diff.successfulActionCount == 1)
        #expect(store.pullRequests[id: 5]?.diff.note == "")
        #expect(store.pullRequestID == 6 && store.pullRequest?.diff.note == "B draft")
        await store.send(.commandResolved(resolution)).finish()
        #expect(store.pullRequests[id: 5]?.diff.successfulActionCount == 1 && calls.value == 1)
        let next = TriageIdentity(server: scope.server, deviceID: "other", credentialRevision: "two", generation: 2)
        await store.send(.triage(.triage(.identityChanged(next)))).finish()
        #expect(store.pullRequests.isEmpty && store.pullRequestID == nil)
        await store.send(.commandResolved(resolution)).finish()
        #expect(store.pullRequests.isEmpty && calls.value == 1)
        started.continuation.finish()
    }
    @Test(.timeLimit(.minutes(1)))
    func identityReplacementCancelsTheActualOwnedOperation() async throws {
        let started = AsyncStream<Void>.makeStream()
        let result = AsyncStream<PRMutationOutcome>.makeStream()
        let terminations = LockIsolated(0)
        result.continuation.onTermination = { _ in terminations.withValue { $0 += 1 } }
        let initialState = try initial()
        let store = Store(initialState: initialState) { ServiceRootFeature() } withDependencies: {
            $0.pullRequestOperations = .init(diff: { _ in .init(diff: "", headSHA: "head") },
                merge: { _, _ in .obsolete }, resolveConflicts: { _ in .obsolete }, setAside: { _ in .obsolete },
                review: { _, _, _, _, _ in
                    started.continuation.yield(())
                    var iterator = result.stream.makeAsyncIterator()
                    return await iterator.next() ?? .obsolete
                })
        }
        await store.send(.focusInbox("p5")).finish()
        await store.send(diff(5, .appeared)).finish()
        await store.send(diff(5, .noteChanged("A"))).finish()
        let operation = store.send(diff(5, .askTapped))
        var iterator = started.stream.makeAsyncIterator()
        _ = await iterator.next()
        let identity = try #require(store.triage.triage.identity)
        await store.send(.triage(.triage(.identityChanged(identity)))).finish()
        #expect(store.pullRequests.count == 1 && terminations.value == 0)
        let next = TriageIdentity(server: identity.server, deviceID: "other", credentialRevision: "two", generation: 2)
        await store.send(.triage(.triage(.identityChanged(next)))).finish()
        await operation.finish()
        #expect(store.pullRequests.isEmpty && store.pullRequestID == nil)
        #expect(terminations.value == 1)
        started.continuation.finish()
    }

    @Test(.timeLimit(.minutes(1)))
    func missingAttachedQuestionAndPRRemainOwnedUntilFinalReconciliation() async throws {
        let question = try JSONDecoder().decode(Question.self, from: Data(#"{"id":1,"sessionId":"s","title":"Proceed?","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","askedAt":1}"#.utf8))
        var state = try initial()
        guard case .pullRequest(let pr) = state.triage.triage.items[0] else { Issue.record("Fixture PR missing"); return }
        state.pullRequest = ServicePullRequestFeature.State(pr: pr, questions: [question])
        state.pullRequestIdentity = state.triage.triage.identity
        let started = AsyncStream<Void>.makeStream()
        let result = AsyncStream<QuestionWorkOutcome>.makeStream()
        let calls = LockIsolated(0)
        let commandID = UUID()
        let store = Store(initialState: state) { ServiceRootFeature() } withDependencies: {
            $0.continuousClock = ImmediateClock()
            $0.questionWorkService = .init(submit: { _ in
                calls.withValue { $0 += 1 }
                started.continuation.yield(())
                var iterator = result.stream.makeAsyncIterator()
                return await iterator.next() ?? .obsolete
            }, document: { _ in "" })
        }
        await store.send(.pullRequests(.element(id: 5, action: .attachedQuestion(.element(id: 1, action: .question(.textChanged("Answer"))))))).finish()
        let operation = store.send(.pullRequests(.element(id: 5, action: .attachedQuestion(.element(id: 1, action: .question(.attachedAnswerTapped))))))
        var iterator = started.stream.makeAsyncIterator()
        _ = await iterator.next()
        await store.send(.pullRequests(.element(id: 5, action: .contextUpdated(reports: [], questions: [])))).finish()
        #expect(store.pullRequests[id: 5]?.visibleQuestionIDs.isEmpty == true)
        #expect(store.pullRequests[id: 5]?.questionFeatures[id: 1]?.question.isSubmitting == true)
        result.continuation.yield(.uncertainCommand(commandID))
        result.continuation.finish()
        await operation.finish()
        let overview = try JSONDecoder().decode(Overview.self, from: Data(#"{"tick":2,"nodes":[],"openQuestions":[],"news":[],"orchestratorMessages":[],"machine":{"memoryTotal":1,"memoryAvailable":1,"load":[0],"diskAvailable":1,"unreliable":false}}"#.utf8))
        let identity = try #require(state.triage.triage.identity)
        let snapshot = ServiceRootFeature.Action.triage(.triage(.inboxUpdated(.init(identity: identity, overview: overview, connection: .online))))
        await store.send(snapshot).finish()
        #expect(store.pullRequests[id: 5]?.questionFeatures[id: 1]?.question.uncertainCommandID == commandID)
        let scope = CommandScope(server: identity.server, deviceID: identity.deviceID, credentialRevision: identity.credentialRevision, generation: identity.generation)
        await store.send(.commandResolved(.init(command: .init(id: commandID, scope: scope, kind: .answer(1), itemID: nil), status: 200))).finish()
        #expect(store.pullRequests[id: 5]?.questionFeatures.isEmpty == true)
        await store.send(snapshot).finish()
        #expect(store.pullRequestID == 5 && store.pullRequest?.displayedPR?.title == "PR5")
        #expect(store.pullRequest?.pr.canAct == false && store.pullRequest?.diff.canReview == false)
        await store.send(.pullRequestDismissed(5)).finish()
        #expect(store.pullRequests.isEmpty && store.pullRequestID == nil && calls.value == 1)
        started.continuation.finish()
    }

}
