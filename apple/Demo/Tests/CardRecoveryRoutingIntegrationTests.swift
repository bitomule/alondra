import AlondraCommands
import AlondraDomain
import AlondraQuestionsFeature
import AlondraShared
import AlondraTriageFeature
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct CardRecoveryRoutingIntegrationTests {
    enum Owner: CaseIterable { case question, document, diff, pullRequest }
    private let scope = CommandScope(server: URL(string: "https://service.example")!, deviceID: "phone", credentialRevision: "one", generation: 1)
    private let commandID = UUID(uuidString: "00000000-0000-0000-0000-000000000091")!
    private let otherID = UUID(uuidString: "00000000-0000-0000-0000-000000000092")!

    private func card(_ owner: Owner, command: UUID) throws -> TriageCardFeature.State {
        let decoder = JSONDecoder()
        let question = try decoder.decode(Question.self, from: Data(#"{"id":1,"sessionId":"s","title":"Proceed?","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","askedAt":1}"#.utf8))
        let report = try decoder.decode(Report.self, from: Data(#"{"id":7,"sessionId":"s","title":"Report","body":"Body","links":[],"attachments":[],"kind":"document","createdAt":1}"#.utf8))
        let pr = try decoder.decode(PullRequest.self, from: Data(#"{"id":5,"sessionId":"s","url":"https://example.com/p/5","repo":"a/b","number":5,"title":"PR","state":"open","openedAt":1}"#.utf8))
        let item: InboxItem = switch owner {
        case .question: .question(question)
        case .document: .report(report)
        case .diff, .pullRequest: .pullRequest(pr)
        }
        var card = TriageCardFeature.State(item: item, identity: nil)
        switch owner {
        case .question:
            card.question?.draft.text = "Keep draft"
            card.question?.needsReconciliation = true
            card.question?.uncertainCommandID = command
        case .document:
            card.document?.draft.general = "Keep draft"
            card.document?.needsReconciliation = true
            card.document?.uncertainCommandID = command
        case .diff:
            card.requestingChanges = true
            card.pullRequest?.diff.note = "Keep draft"
            card.pullRequest?.diff.submittedNote = "Keep draft"
            card.pullRequest?.diff.mutationOutcome = .unknown(command)
        case .pullRequest:
            card.pullRequest?.pr.mutationOutcome = .unknown(command)
        }
        return card
    }

    private func pendingID(_ card: TriageCardFeature.State, owner: Owner) -> UUID? {
        switch owner {
        case .question: card.question?.uncertainCommandID
        case .document: card.document?.uncertainCommandID
        case .diff: card.pullRequest?.diff.mutationOutcome?.uncertainID
        case .pullRequest: card.pullRequest?.pr.mutationOutcome?.uncertainID
        }
    }

    private func resolution(_ id: UUID, owner: Owner, succeeded: Bool) -> CommandResolution {
        let kind: CommandKind = switch owner {
        case .question: .answer(1)
        case .document: .review(7)
        case .diff: .requestChanges(5)
        case .pullRequest: .mergePullRequest(5)
        }
        return CommandResolution(command: .init(id: id, scope: scope, kind: kind, itemID: nil), status: succeeded ? 200 : 422)
    }

    @Test(arguments: Owner.allCases, [false, true])
    func rootRoutesOnlyTheOutstandingCardCommand(owner: Owner, succeeded: Bool) async throws {
        let card = try card(owner, command: commandID)
        var initial = ServiceRootFeature.State()
        initial.triage.cards = [card]
        // A second mounted card waits on an unrelated command. A broadcast must
        // never acknowledge it or discard its draft.
        var unrelated = try self.card(.document, command: otherID)
        if owner == .document { unrelated = try self.card(.question, command: otherID) }
        initial.triage.cards.append(unrelated)
        let store = Store(initialState: initial) { ServiceRootFeature() } withDependencies: {
            $0.continuousClock = ImmediateClock()
        }
        await store.send(.commandResolved(resolution(UUID(uuidString: "00000000-0000-0000-0000-000000000093")!, owner: owner, succeeded: false))).finish()
        #expect(pendingID(try #require(store.triage.cards[id: card.id]), owner: owner) == commandID)
        let targetResult = resolution(commandID, owner: owner, succeeded: succeeded)
        await store.send(.commandResolved(targetResult)).finish()
        let resolved = try #require(store.triage.cards[id: card.id])
        #expect(pendingID(resolved, owner: owner) == nil)
        #expect(store.triage.triage.hiddenIDs.contains(card.id) == succeeded)
        switch owner {
        case .question:
            #expect(resolved.question?.needsReconciliation == false)
            #expect(resolved.question?.draft.text == (succeeded ? "" : "Keep draft"))
        case .document:
            #expect(resolved.document?.needsReconciliation == false)
            #expect(resolved.document?.draft.general == (succeeded ? "" : "Keep draft"))
        case .diff:
            #expect(resolved.pullRequest?.diff.note == (succeeded ? "" : "Keep draft"))
        case .pullRequest:
            #expect(resolved.pullRequest?.pr.mutationOutcome == (succeeded ? .confirmed : .rejected(targetResult.message)))
        }
        let untouched = try #require(store.triage.cards[id: unrelated.id])
        #expect(pendingID(untouched, owner: owner == .document ? .question : .document) == otherID)
        #expect(untouched.reply == "Keep draft")
        let resolvedCount = store.triage.triage.resolvedCount
        await store.send(.commandResolved(targetResult)).finish()
        #expect(store.triage.triage.resolvedCount == resolvedCount, "Replay must not resolve the card twice")
    }

    @Test
    func lateRecoveryAfterIdentityReplacementDoesNotRecreateRemovedCards() async throws {
        var initial = ServiceRootFeature.State()
        initial.triage.cards = [try card(.question, command: commandID)]
        let store = Store(initialState: initial) { ServiceRootFeature() }
        await store.send(.triage(.triage(.identityChanged(.init(server: scope.server, deviceID: "replacement", credentialRevision: "two", generation: 2))))).finish()
        #expect(store.triage.cards.isEmpty)
        await store.send(.commandResolved(resolution(commandID, owner: .question, succeeded: true))).finish()
        #expect(store.triage.cards.isEmpty)
        #expect(store.triage.triage.hiddenIDs.isEmpty)
        #expect(store.notice == nil)
    }

    @Test
    func cachedFeedbackRecoveryReachesCardAndReadRetryNeverResendsMessage() async throws {
        // Real transaction produces its public recovery events; no test-only DTO
        // initializer or fabricated child commandResolved bypasses root routing.
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let journal = FileCommandJournal(directory: directory.appendingPathComponent("commands"))
        let transaction = ReportFeedbackTransaction(directory: directory.appendingPathComponent("feedback"),
            gate: CommandGate(scope: scope, journal: journal), scope: scope)
        let sends = LockIsolated(0)
        let statuses = LockIsolated<[String: ReportFeedbackStatus]>([:])
        let result = await transaction.submit(reportID: 7, binding: Data("draft-binding".utf8), sendMessage: { _ in
            sends.withValue { $0 += 1 }
        }, markRead: { id in
            statuses.withValue { $0[id] = .unknown }
            throw URLError(.networkConnectionLost)
        }, reconcile: { id in statuses.value[id] ?? .notDispatched })
        guard case .unknown(let command) = result else { Issue.record("Expected delivered feedback awaiting read"); return }
        let stillUnknown = try await transaction.recover(markRead: { _, _ in Issue.record("Read must remain status-only") }, reconcile: { _ in .unknown })
        #expect(stillUnknown.first?.outcome == .unknown)
        let rejectedRead = try await transaction.recover(markRead: { _, _ in Issue.record("Do not redispatch unconfirmed read") }, reconcile: { _ in .rejected(422) })
        #expect(rejectedRead.first?.outcome == .readRejected(422))
        var card = try self.card(.document, command: command.id)
        card.document?.needsReconciliation = false
        card.document?.uncertainCommandID = nil
        card.document?.isSubmitting = true
        var initial = ServiceRootFeature.State()
        initial.triage.cards = [card]
        var unrelatedDetail = try #require(self.card(.document, command: otherID).document)
        unrelatedDetail.draft.general = "Other outstanding feedback"
        initial.triage.document = unrelatedDetail
        let retried = LockIsolated<[ReportFeedbackRecoveryEvent]>([])
        let readIDs = LockIsolated<[String]>([])
        let store = Store(initialState: initial) { ServiceRootFeature() } withDependencies: {
            $0.continuousClock = ImmediateClock()
            $0.questionWorkService = .init(submit: { _ in Issue.record("Recovery must not submit feedback"); return .obsolete }, document: { _ in "" }, retryReportRead: { id in
                do {
                    let events = try await transaction.recover(retryingReadFor: id, markRead: { report, readID in
                        #expect(report == 7)
                        readIDs.withValue { $0.append(readID) }
                    }, reconcile: { _ in .notDispatched })
                    retried.setValue(events)
                } catch { Issue.record("Read retry failed: \(error)") }
            })
        }
        await store.send(.feedbackRecovered(stillUnknown)).finish()
        #expect(store.triage.cards[id: card.id]?.document?.isSubmitting == true)
        #expect(store.triage.document?.uncertainCommandID == otherID)
        // Recovery can arrive before the asynchronous submission result installs
        // its UUID. The root must replay its cache after that accepted result.
        await store.send(.feedbackRecovered(rejectedRead)).finish()
        #expect(store.triage.cards[id: card.id]?.document?.feedbackReadRetryAvailable == false)
        await store.send(.triage(.cards(.element(id: card.id, action: .document(.reportFeedbackFinished(0, .uncertainCommand(command.id))))))).finish()
        let waiting = try #require(store.triage.cards[id: card.id]?.document)
        #expect(waiting.needsReconciliation)
        #expect(waiting.feedbackReadRetryAvailable)
        #expect(waiting.draft.general == "Keep draft")
        await store.send(.triage(.cards(.element(id: card.id, action: .document(.retryFeedbackRead))))).finish()
        #expect(readIDs.value == [command.id.uuidString + "-read-1"])
        #expect(sends.value == 1)
        await store.send(.feedbackRecovered(retried.value)).finish()
        let finished = try #require(store.triage.cards[id: card.id]?.document)
        #expect(store.triage.document?.uncertainCommandID == otherID)
        #expect(store.triage.document?.draft.general == "Other outstanding feedback")
        #expect(finished.completed)
        #expect(!finished.needsReconciliation)
        #expect(finished.draft.general.isEmpty)
        #expect(store.triage.triage.hiddenIDs.contains(card.id))
        #expect(sends.value == 1)
    }
}
