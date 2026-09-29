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
    nonisolated static let receipt = DomainV2.AttachmentRef(id: "receipt", name: "proof.txt", contentDigest: Data("proof".utf8).sha256Hex,
                                                            contentReference: "ref", bytes: 5)

    private func fixture(questionOnFive: Bool = false) -> V2Fixture {
        var fixture = V2Fixture()
        fixture.pullRequest(5, questions: questionOnFive ? ["q1"] : [])
        fixture.pullRequest(6)
        return fixture
    }

    private func diff(_ number: Int, _ action: DiffReviewFeature.Action) -> ServiceRootFeature.Action {
        .pullRequests(.element(id: .init(rawValue: "pr-\(number)"), action: .diff(action)))
    }

    private func focus(_ number: Int) -> ServiceRootFeature.Action { .focusInbox("pr:pr-\(number)") }

    @Test(.timeLimit(.minutes(1)))
    func navigationRetainsDraftAndUploadReceiptWithoutSendingAbandonedIntent() async throws {
        let file = AttachmentDraft(name: "proof.txt", data: Data("proof".utf8))
        let started = AsyncStream<Void>.makeStream()
        let receipt = AsyncStream<DomainV2.AttachmentRef>.makeStream()
        let imported = AsyncStream<Void>.makeStream()
        let log = CommandLog()
        let store = Store(initialState: ServiceRootFeature.State()) {
            CombineReducers {
                ServiceRootFeature()
                Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> { _, action in
                    if case .pullRequests(.element("pr-5", .diff(.attachmentPicker(.importFinished)))) = action {
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
                guard let value = await iterator.next() else { throw CancellationError() }
                return value
            })
            $0.domainWorkspace = .scripted(log) { intent in
                guard case let .askAboutPullRequest("pr-5", _, body, attachments) = intent else {
                    Issue.record("Unexpected command \(intent)"); return .obsolete
                }
                #expect(body.contains("Keep A") && attachments == [Self.receipt])
                return V2Fixture.rejected("Retain draft")
            }
        }
        await store.send(.triage(.triage(.workspaceUpdated(fixture().update())))).finish()
        await store.send(focus(5)).finish()
        await store.send(diff(5, .appeared)).finish()
        await store.send(diff(5, .noteChanged("Keep A"))).finish()
        let upload = store.send(diff(5, .attachmentPicker(.filesSelected([URL(fileURLWithPath: "/fixture/proof.txt")]))))
        var iterator = started.stream.makeAsyncIterator()
        _ = await iterator.next()
        var completedImport = imported.stream.makeAsyncIterator()
        _ = await completedImport.next()
        await store.send(diff(5, .askTapped)).finish()
        #expect(store.pullRequests[id: "pr-5"]?.diff.pendingReviewKind != nil)
        await store.send(focus(6)).finish()
        await store.send(diff(5, .disappeared)).finish()
        #expect(store.pullRequestID == "pr-6")
        #expect(store.pullRequests[id: "pr-5"]?.diff.pendingReviewKind == nil)
        receipt.continuation.yield(Self.receipt)
        receipt.continuation.finish()
        await upload.finish()
        #expect(log.count == 0)
        #expect(store.pullRequests[id: "pr-5"]?.diff.note == "Keep A")
        #expect(store.pullRequests[id: "pr-5"]?.diff.attachmentPicker.stored == [Self.receipt])
        await store.send(.pullRequestDismissed("pr-6")).finish()
        #expect(store.pullRequest == nil && store.pullRequests.count == 2)
        await store.send(focus(5)).finish()
        #expect(store.pullRequest?.diff.note == "Keep A")
        await store.send(diff(5, .askTapped)).finish()
        #expect(log.count == 1)
        #expect(store.pullRequest?.diff.note == "Keep A")
        started.continuation.finish()
        imported.continuation.finish()
    }

    @Test(.timeLimit(.minutes(1)))
    func dispatchedReviewStaysOwnedAndIsRetriedWhileAnotherPRIsVisible() async throws {
        let gate = CommandGate()
        let log = CommandLog()
        let results = LockIsolated<[DomainCommandResult]>([])
        let store = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() } withDependencies: {
            $0.continuousClock = ImmediateClock()
            $0.uuid = .incrementing
            $0.domainWorkspace = .scripted(log) { _ in
                if let next = results.withValue({ $0.isEmpty ? nil : $0.removeFirst() }) { return next }
                return await gate.run()
            }
        }
        await store.send(.triage(.triage(.workspaceUpdated(fixture().update())))).finish()
        await store.send(focus(5)).finish()
        await store.send(diff(5, .appeared)).finish()
        await store.send(diff(5, .noteChanged("A in flight"))).finish()
        let command = store.send(diff(5, .askTapped))
        await gate.waitUntilStarted()
        await store.send(.pullRequestDismissed("pr-5")).finish()
        await store.send(diff(5, .disappeared)).finish()
        await store.send(focus(6)).finish()
        await store.send(.pullRequestDismissed("pr-5")).finish()
        #expect(store.pullRequestID == "pr-6")
        await store.send(diff(6, .noteChanged("B draft"))).finish()
        await gate.release(.uncertain)
        await command.finish()
        #expect(store.pullRequests[id: "pr-5"]?.diff.mutationOutcome == .unknown)
        #expect(store.pullRequests[id: "pr-5"]?.diff.note == "A in flight")
        #expect(store.pullRequestID == "pr-6" && store.pullRequest?.diff.note == "B draft")
        results.withValue { $0 = [V2Fixture.recorded] }
        await store.send(focus(5)).finish()
        await store.send(diff(5, .askTapped)).finish()
        #expect(log.count == 2)
        #expect(Set(log.commandIDs).count == 1, "the retry is the same command")
        #expect(store.pullRequests[id: "pr-5"]?.diff.successfulActionCount == 1)
        #expect(store.pullRequests[id: "pr-5"]?.diff.note == "")
        await store.send(focus(6)).finish()
        #expect(store.pullRequestID == "pr-6" && store.pullRequest?.diff.note == "B draft")
        await store.send(.triage(.triage(.identityChanged(V2Fixture.other)))).finish()
        #expect(store.pullRequests.isEmpty && store.pullRequestID == nil)
        #expect(log.count == 2)
    }

    @Test(.timeLimit(.minutes(1)))
    func identityReplacementCancelsTheActualOwnedOperation() async throws {
        let started = AsyncStream<Void>.makeStream()
        let result = AsyncStream<DomainCommandResult>.makeStream()
        let terminations = LockIsolated(0)
        result.continuation.onTermination = { _ in terminations.withValue { $0 += 1 } }
        let log = CommandLog()
        let store = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() } withDependencies: {
            $0.domainWorkspace = .scripted(log) { _ in
                started.continuation.yield(())
                var iterator = result.stream.makeAsyncIterator()
                return await iterator.next() ?? .obsolete
            }
        }
        await store.send(.triage(.triage(.workspaceUpdated(fixture().update())))).finish()
        await store.send(focus(5)).finish()
        await store.send(diff(5, .appeared)).finish()
        await store.send(diff(5, .noteChanged("A"))).finish()
        let operation = store.send(diff(5, .askTapped))
        var iterator = started.stream.makeAsyncIterator()
        _ = await iterator.next()
        let identity = try #require(store.triage.triage.identity)
        await store.send(.triage(.triage(.identityChanged(identity)))).finish()
        #expect(store.pullRequests.count == 1 && terminations.value == 0)
        await store.send(.triage(.triage(.identityChanged(V2Fixture.other)))).finish()
        await operation.finish()
        #expect(store.pullRequests.isEmpty && store.pullRequestID == nil)
        #expect(terminations.value == 1)
        started.continuation.finish()
    }

    @Test(.timeLimit(.minutes(1)))
    func anAttachedQuestionAndItsPRStayOwnedUntilTheirCommandSettles() async throws {
        let gate = CommandGate()
        let log = CommandLog()
        let results = LockIsolated<[DomainCommandResult]>([])
        let store = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() } withDependencies: {
            $0.continuousClock = ImmediateClock()
            $0.uuid = .incrementing
            $0.domainWorkspace = .scripted(log) { _ in
                if let next = results.withValue({ $0.isEmpty ? nil : $0.removeFirst() }) { return next }
                return await gate.run()
            }
        }
        func question(_ action: QuestionFeature.Action) -> ServiceRootFeature.Action {
            .pullRequests(.element(id: "pr-5", action: .attachedQuestion(.element(id: "q1", action: .question(action)))))
        }
        await store.send(.triage(.triage(.workspaceUpdated(fixture(questionOnFive: true).update())))).finish()
        await store.send(focus(5)).finish()
        #expect(store.pullRequest?.visibleQuestionIDs == ["q1"])
        await store.send(question(.optionSelected("reply"))).finish()
        await store.send(question(.textChanged("Answer"))).finish()
        let operation = store.send(question(.answerTapped))
        await gate.waitUntilStarted()
        // The server answered it elsewhere: the next projection no longer asks it.
        await store.send(.triage(.triage(.workspaceUpdated(fixture().update())))).finish()
        #expect(store.pullRequests[id: "pr-5"]?.visibleQuestionIDs.isEmpty == true)
        #expect(store.pullRequests[id: "pr-5"]?.questionFeatures[id: "q1"]?.question.isSubmitting == true)
        await gate.release(.uncertain)
        await operation.finish()
        #expect(store.pullRequests[id: "pr-5"]?.questionFeatures[id: "q1"]?.question.needsReconciliation == true)
        await store.send(.triage(.triage(.workspaceUpdated(fixture().update())))).finish()
        #expect(store.pullRequests[id: "pr-5"]?.questionFeatures[id: "q1"] != nil, "an unknown outcome keeps its draft and command")
        results.withValue { $0 = [V2Fixture.recorded] }
        await store.send(question(.answerTapped)).finish()
        #expect(log.count == 2 && Set(log.commandIDs).count == 1)
        #expect(store.pullRequests[id: "pr-5"]?.questionFeatures.isEmpty == true)
        // The PR itself leaves the projection while its detail is open: the detail stays, inert.
        var merged = V2Fixture()
        merged.pullRequest(6)
        await store.send(.triage(.triage(.workspaceUpdated(merged.update())))).finish()
        #expect(store.pullRequestID == "pr-5")
        #expect(store.pullRequest?.pr.canAct == false && store.pullRequest?.diff.canReview == false)
        await store.send(.pullRequestDismissed("pr-5")).finish()
        #expect(store.pullRequests.isEmpty && store.pullRequestID == nil && log.count == 2)
    }
}
