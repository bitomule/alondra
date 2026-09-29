import AlondraDomain
import AlondraAttachmentsFeature
import AlondraFleetFeature
import AlondraNewWorkFeature
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct NewFleetNoticeIntegrationTests {
    @Test
    func failedUploadDuringNewSendRetainsFileAndNeverDispatchesRequest() async throws {
        let draft = AttachmentDraft(name: "proof.txt", data: Data("proof".utf8))
        var initial = ServiceRootFeature.State()
        initial.newRequest.text = "Keep this request"
        initial.newRequest.canSubmit = true
        initial.newRequest.catalogIsCurrent = true
        initial.newRequest.attachmentPicker.entries = [.init(draft: draft, status: .failed("Offline"))]
        let uploads = LockIsolated(0)
        let creates = LockIsolated(0)
        let reduced = AsyncStream<Void>.makeStream()
        let clock = TestClock()
        let store = Store(initialState: initial) {
            CombineReducers {
                ServiceRootFeature()
                Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> { _, action in
                    if case .newRequest(.attachmentPicker(.uploaded)) = action { reduced.continuation.yield(()) }
                    return .none
                }
            }
        } withDependencies: {
            $0.continuousClock = clock
            $0.attachmentUpload = .init(upload: { _ in
                uploads.withValue { $0 += 1 }
                throw AttachmentFailure(message: "Still offline")
            })
            $0.newWorkOperations = .init(
                projects: { [] }, options: { _ in fatalError("Unexpected catalogue read") },
                issues: { _ in fatalError("Unexpected issue read") },
                captureSubmissionScope: {
                    creates.withValue { $0 += 1 }
                    return nil
                })
        }
        let submission = store.send(.newRequest(.submitTapped))
        var iterator = reduced.stream.makeAsyncIterator()
        _ = await iterator.next()
        #expect(uploads.value == 1)
        #expect(creates.value == 0)
        #expect(store.newRequest.pendingSubmission == nil)
        #expect(store.newRequest.attachmentPicker.entries.first?.draft == draft)
        #expect(store.newRequest.attachmentPicker.entries.first?.status == .failed("Still offline"))
        #expect(store.newRequest.text == "Keep this request")
        #expect(store.newRequest.inlineSubmissionError == nil)
        #expect(store.notice?.event == .failure(couldNotSendMessage("Still offline")))
        await clock.advance(by: .seconds(6))
        await submission.finish()
        #expect(store.notice == nil)
        #expect(store.newRequest.inlineSubmissionError == nil)
        reduced.continuation.finish()
    }

    @Test
    func rejectedNewRequestKeepsDraftAndShowsOnlyTransientWorkspaceFailure() async throws {
        var initial = ServiceRootFeature.State()
        initial.newRequest.text = "Keep this request"
        initial.newRequest.canSubmit = true
        initial.newRequest.catalogIsCurrent = true
        let sent = LockIsolated<[NewRequestBody]>([])
        let reduced = AsyncStream<Void>.makeStream()
        let clock = TestClock()
        let store = Store(initialState: initial) {
            CombineReducers {
                ServiceRootFeature()
                Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> { _, action in
                    if case .newRequest(.submitFinished) = action { reduced.continuation.yield(()) }
                    return .none
                }
            }
        } withDependencies: {
            $0.continuousClock = clock
            $0.newWorkOperations = .init(
                projects: { [] }, options: { _ in fatalError("Unexpected catalogue read") },
                issues: { _ in fatalError("Unexpected issue read") },
                captureSubmissionScope: {
                    .init(upload: { _ in fatalError("No attachments") }, isCurrent: { true }, create: { body, _ in
                        sent.withValue { $0.append(body) }
                        return .failed("Rejected")
                    })
                })
        }
        let submission = store.send(.newRequest(.submitTapped))
        var iterator = reduced.stream.makeAsyncIterator()
        _ = await iterator.next()
        #expect(sent.value.count == 1)
        #expect(store.newRequest.text == "Keep this request")
        #expect(store.newRequest.inlineSubmissionError == nil)
        #expect(store.newRequest.submitEnabled)
        #expect(store.notice?.event == .failure(couldNotSendMessage("Rejected")))
        await clock.advance(by: .seconds(6))
        await submission.finish()
        #expect(store.notice == nil)
        #expect(store.newRequest.inlineSubmissionError == nil)
        #expect(store.newRequest.text == "Keep this request")
        reduced.continuation.finish()
    }

    @Test
    func fleetListCloseRejectionReachesOriginalAlertAndStaleResultsCannotReopenIt() async throws {
        let node = try JSONDecoder().decode(Node.self, from: Data(#"{"sessionId":"s1","shortId":"s1","name":"Worker","project":"Alondra","state":"working","stateSince":1,"isOrchestrator":false,"hasPlugin":true,"stateSource":"hook"}"#.utf8))
        let entry = FleetNode(node: node, tasks: [], pendingCount: 0)
        var initial = ServiceRootFeature.State()
        initial.fleet.fleet.snapshot = FleetSnapshot(sequence: 1, tick: 1, orchestrator: nil,
            sections: [.init(name: "Alondra", nodes: [entry])])
        let closed = LockIsolated<[String]>([])
        let store = Store(initialState: initial) { ServiceRootFeature() } withDependencies: {
            $0.nodeOperations = .init(send: { _, _, _ in fatalError("Unexpected send") }, close: { id, _ in
                closed.withValue { $0.append(id) }
                return .rejected("Worktree still present")
            }, threads: { _ in [] })
        }
        await store.send(.fleet(.fleet(.closeRequested("s1")))).finish()
        await store.send(.fleet(.fleet(.closeConfirmed))).finish()
        #expect(closed.value == ["s1"])
        #expect(store.closeRefusal == "Worktree still present")
        #expect(store.notice == nil)
        await store.send(.dismissCloseRefusal).finish()
        await store.send(.fleet(.fleet(.closeFinished("s1", .rejected("Late duplicate"), 0)))).finish()
        #expect(store.closeRefusal == nil)
        #expect(store.fleet.fleet.closeOutcomeByID["s1"] == .rejected("Worktree still present"))
    }
}
