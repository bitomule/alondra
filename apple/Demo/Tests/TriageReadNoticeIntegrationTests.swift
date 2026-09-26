import AlondraDomain
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct TriageReadNoticeIntegrationTests {
    @Test(.timeLimit(.minutes(1)), arguments: [false, true])
    func rejectedReadRestoresCardAndShowsOriginalToast(deferred: Bool) async throws {
        let decoder = JSONDecoder()
        let report = try decoder.decode(Report.self, from: Data(#"{"id":7,"sessionId":"s","title":"Report","links":[],"attachments":[],"kind":"final","createdAt":1}"#.utf8))
        let identity = TriageIdentity(server: URL(string: "https://service.example")!, deviceID: "phone", credentialRevision: "one", generation: 1)
        var initial = ServiceRootFeature.State()
        initial.triage.triage.identity = identity
        initial.triage.triage.items = [.report(report)]
        let id = InboxItem.report(report).id
        let calls = LockIsolated(0)
        let observed = AsyncStream<Void>.makeStream()
        let clock = TestClock()
        let store = Store(initialState: initial) {
            CombineReducers {
                ServiceRootFeature()
                Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> { _, action in
                    if case .triage(.triage(.mutationFinished)) = action { observed.continuation.yield(()) }
                    return .none
                }
            }
        } withDependencies: {
            $0.continuousClock = clock
            $0.triageService = .init(observe: { AsyncStream { $0.finish() } }, mutate: { kind, item, scope in
                #expect(kind == .markRead(7) && item == id && scope == identity)
                calls.withValue { $0 += 1 }
                return .rejected("Read rejected")
            })
        }
        let operation = store.send(.triage(.triage(deferred ? .readWithPresentation(id) : .hide(id))))
        var iterator = observed.stream.makeAsyncIterator()
        _ = await iterator.next()
        #expect(calls.value == 1)
        #expect(!store.triage.triage.hiddenIDs.contains(id))
        #expect(store.triage.triage.queue.contains { $0.id == id })
        #expect(store.triage.triage.noticeIsMutationFailure)
        #expect(store.notice?.event == .failure(couldNotSendMessage("Read rejected")))
        await clock.advance(by: .seconds(6))
        await operation.finish()
        await store.send(.triage(.triage(.mutationFinished(id, identity, .rejected("Late"))))).finish()
        #expect(store.notice == nil && calls.value == 1)
        observed.continuation.finish()
    }
}
