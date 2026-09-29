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
        var fixture = V2Fixture()
        let item = fixture.information("info-7")
        let id = item.id
        let identity = V2Fixture.identity
        let log = CommandLog()
        let observed = AsyncStream<Void>.makeStream()
        let clock = TestClock()
        let store = Store(initialState: ServiceRootFeature.State()) {
            CombineReducers {
                ServiceRootFeature()
                Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> { _, action in
                    if case .triage(.triage(.mutationFinished)) = action { observed.continuation.yield(()) }
                    return .none
                }
            }
        } withDependencies: {
            $0.continuousClock = clock
            $0.domainWorkspace = .scripted(log) { intent in
                #expect(intent == .markRead("info-7"))
                return V2Fixture.rejected("Read rejected")
            }
        }
        await store.send(.triage(.triage(.workspaceUpdated(fixture.update())))).finish()
        var iterator = observed.stream.makeAsyncIterator()
        var previousMacNotice: UUID?
        for attempt in 1...2 {
            let operation = store.send(.triage(.triage(deferred ? .readWithPresentation(id) : .hide(id))))
            _ = await iterator.next()
            #expect(log.count == attempt)
            #expect(!store.triage.triage.hiddenIDs.contains(id))
            #expect(store.triage.triage.queue.contains { $0.id == id })
            #expect(store.triage.triage.noticeIsMutationFailure)
            let expected = WorkspaceNotice.Event.failure(couldNotSendMessage("Read rejected"))
            #expect(store.notice?.event == expected)
            #expect(store.macError?.event == expected)
            let macNotice = try #require(store.macError?.id)
            #expect(macNotice != previousMacNotice)
            previousMacNotice = macNotice
            await clock.advance(by: .seconds(6))
            await operation.finish()
            #expect(store.notice == nil && store.macError == nil)
            // The retained leaf result is data, not an error that can resurrect
            // a toast on an unrelated action after its normal timeout.
            await store.send(.settingsPresented(false)).finish()
            #expect(store.notice == nil && store.macError == nil)
        }
        #expect(Set(log.commandIDs).count == 2, "A rejected read is final: trying again is a new command")
        await store.send(.triage(.triage(.mutationFinished(id, identity, .rejected("Late"))))).finish()
        #expect(store.notice == nil && store.macError == nil && log.count == 2)
        observed.continuation.finish()
    }
}
