import AlondraDomain
import AlondraShared
import AlondraTasksFeature
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct TaskRejectionNoticeIntegrationTests {
    enum Route: CaseIterable { case archive, withdraw, cancelSheet, reopenSheet, cancelPath, reopenPath }

    @Test(.timeLimit(.minutes(1)), arguments: Route.allCases, [false, true])
    func taskFailuresUseToastAndUnknownOutcomesRetryTheSameCommand(route: Route, uncertain: Bool) async throws {
        let finished = route == .reopenSheet || route == .reopenPath || route == .archive
        var fixture = V2Fixture()
        fixture.tasks = route == .withdraw ? [] : [V2Fixture.makeTask("t1", state: finished ? .done : .running)]
        let actions = finished ? [fixture.action(.reopenTask), fixture.action(.archiveTask)] : [fixture.action(.cancelTask)]
        let log = CommandLog()
        let observed = AsyncStream<Void>.makeStream()
        let loaded = AsyncStream<Void>.makeStream()
        let clock = TestClock()
        let result = uncertain ? DomainCommandResult.uncertain : V2Fixture.rejected()
        let store = Store(initialState: ServiceRootFeature.State()) {
            CombineReducers {
                ServiceRootFeature()
                Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> { _, action in
                    switch action {
                    case .tasks(.board(.archiveFinished)), .tasks(.withdrawFinished),
                         .tasks(.detail(.presented(.mutationFinished))),
                         .tasks(.taskPath(.element(_, .mutationFinished))): observed.continuation.yield(())
                    case .tasks(.detail(.presented(.actionsLoaded))), .tasks(.taskPath(.element(_, .actionsLoaded))):
                        loaded.continuation.yield(())
                    default: break
                    }
                    return .none
                }
            }
        } withDependencies: {
            $0.continuousClock = clock
            $0.uuid = .incrementing
            $0.domainWorkspace = .scripted(log, taskActions: actions) { _ in result }
        }
        await store.send(.triage(.triage(.workspaceUpdated(fixture.update())))).finish()
        // Presenting a detail keeps an effect alive until it is dismissed, so the test waits
        // for the server's task actions instead of for the presentation to end.
        var actionsLoaded = loaded.stream.makeAsyncIterator()
        let presentation: StoreTask?
        switch route {
        case .cancelSheet, .reopenSheet: presentation = store.send(.tasks(.openTask("t1")))
        case .cancelPath, .reopenPath: presentation = store.send(.tasks(.pushTask("t1")))
        case .archive, .withdraw: presentation = nil
        }
        if presentation != nil { _ = await actionsLoaded.next() }
        let pathID = store.tasks.taskPath.ids.first
        func detailAction(_ action: TaskDetailFeature.Action) -> ServiceRootFeature.Action {
            if let pathID { return .tasks(.taskPath(.element(id: pathID, action: action))) }
            return .tasks(.detail(.presented(action)))
        }
        func submit() async throws -> StoreTask {
            switch route {
            case .archive:
                try #require(store.tasks.board.canArchive)
                return store.send(.tasks(.board(.archiveTapped)))
            case .withdraw:
                await store.send(.tasks(.withdrawRequested(V2Fixture.request))).finish()
                try #require(store.tasks.withdrawingRequestID == V2Fixture.request)
                return store.send(.tasks(.withdrawConfirmed))
            case .cancelSheet, .cancelPath:
                await store.send(detailAction(.cancelTapped)).finish()
                return store.send(detailAction(.cancelConfirmed))
            case .reopenSheet, .reopenPath:
                return store.send(detailAction(.reopenTapped))
            }
        }
        let expected: DomainIntent = switch route {
        case .archive: .archiveTask("t1", archived: true)
        case .withdraw: .closeRequest(V2Fixture.request)
        case .cancelSheet, .cancelPath: .cancelTask("t1")
        case .reopenSheet, .reopenPath: .reopenTask("t1")
        }
        var iterator = observed.stream.makeAsyncIterator()
        let operation = try await submit()
        _ = await iterator.next()
        #expect(log.intents == [expected])
        if uncertain {
            #expect(store.notice == nil)
            await operation.finish()
            let retry = try await submit()
            _ = await iterator.next()
            await retry.finish()
            #expect(log.intents == [expected, expected])
            #expect(Set(log.commandIDs).count == 1, "trying again after an unknown outcome is the same command")
            #expect(store.notice == nil)
        } else {
            #expect(store.notice?.event == .failure(couldNotSendMessage("Rejected")))
            await clock.advance(by: .seconds(6))
            await operation.finish()
        }
        #expect(store.notice == nil)
        // Duplicate and stale completions cannot create a second toast.
        for epoch in [UInt64.max, 0] {
            let stale: ServiceRootFeature.Action
            switch route {
            case .archive: stale = .tasks(.board(.archiveFinished(["t1": .rejected("Late")], epoch)))
            case .withdraw: stale = .tasks(.withdrawFinished(V2Fixture.request, .rejected("Late")))
            default: stale = detailAction(.mutationFinished(.rejected("Late"), epoch))
            }
            await store.send(stale).finish()
            #expect(store.notice == nil)
        }
        #expect(log.count == (uncertain ? 2 : 1))
        if let pathID { await store.send(.tasks(.taskPath(.popFrom(id: pathID)))).finish() }
        if store.tasks.detail != nil { await store.send(.tasks(.detail(.dismiss))).finish() }
        await presentation?.finish()
        observed.continuation.finish()
        loaded.continuation.finish()
    }
}
