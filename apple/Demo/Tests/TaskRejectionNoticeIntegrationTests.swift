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
    func taskFailuresUseToastAndUncertainCommandsStayBlocked(route: Route, uncertain: Bool) async throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let reopening = route == .reopenSheet || route == .reopenPath
        let task = try decoder.decode(WorkTask.self, from: Data("""
        {"id":"t-1","request":"r-1","title":"Task","state":"\(reopening ? "done" : "running")","state_since":1,"created_at":1}
        """.utf8))
        let request = try decoder.decode(WorkRequest.self, from: Data(#"{"id":"r-1","text":"Request","created_at":1,"state":"in_progress","tasks":["t-1"]}"#.utf8))
        var initial = ServiceRootFeature.State()
        initial.tasks.taskBoard = TaskBoard(requests: [request], tasks: [task])
        initial.tasks.board.canArchive = true
        var detail = TaskDetailFeature.State(taskID: task.id)
        detail.task = task
        initial.tasks.detail = detail
        initial.tasks.taskPath.append(detail)
        let pathID = try #require(initial.tasks.taskPath.ids.first)
        func detailAction(_ action: TaskDetailFeature.Action) -> ServiceRootFeature.Action {
            switch route {
            case .cancelPath, .reopenPath: return .tasks(.taskPath(.element(id: pathID, action: action)))
            default: return .tasks(.detail(.presented(action)))
            }
        }
        func resolved(_ id: UUID) -> ServiceRootFeature.Action {
            switch route {
            case .archive: return .tasks(.board(.commandResolved(id, rejection: "Rejected")))
            case .withdraw: return .tasks(.withdrawalResolved(id, rejection: "Rejected"))
            default: return detailAction(.commandResolved(id, rejection: "Rejected"))
            }
        }
        let command = UUID()
        let result: TaskMutationOutcome = uncertain ? .unknown(command) : .rejected("Rejected")
        let calls = LockIsolated<[String]>([])
        let observed = AsyncStream<Void>.makeStream()
        let clock = TestClock()
        let store = Store(initialState: initial) {
            CombineReducers {
                ServiceRootFeature()
                Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> { _, action in
                    switch action {
                    case .tasks(.board(.archiveFinished)), .tasks(.withdrawFinished),
                         .tasks(.detail(.presented(.mutationFinished))),
                         .tasks(.taskPath(.element(_, .mutationFinished))): observed.continuation.yield(())
                    default: break
                    }
                    return .none
                }
            }
        } withDependencies: {
            $0.continuousClock = clock
            $0.taskOperations = TaskOperations(
                withdraw: { value in #expect(value.id == "r-1"); calls.withValue { $0.append("withdraw") }; return result },
                cancel: { value in #expect(value.id == "t-1"); calls.withValue { $0.append("cancel") }; return result },
                reopen: { value in #expect(value.id == "t-1"); calls.withValue { $0.append("reopen") }; return result },
                archiveCompleted: { calls.withValue { $0.append("archive") }; return result }, details: { _ in nil })
        }
        let submit: ServiceRootFeature.Action
        switch route {
        case .archive: submit = .tasks(.board(.archiveTapped))
        case .withdraw:
            await store.send(.tasks(.withdrawRequested("r-1"))).finish()
            submit = .tasks(.withdrawConfirmed)
        default:
            if !reopening { await store.send(detailAction(.cancelTapped)).finish() }
            submit = detailAction(reopening ? .reopenTapped : .cancelConfirmed)
        }
        let operation = store.send(submit)
        var iterator = observed.stream.makeAsyncIterator()
        _ = await iterator.next()
        let expected = route == .archive ? "archive" : route == .withdraw ? "withdraw" : reopening ? "reopen" : "cancel"
        #expect(calls.value == [expected])
        if uncertain {
            #expect(store.notice == nil)
            await store.send(submit).finish()
            #expect(calls.value == [expected])
            await store.send(resolved(UUID())).finish()
            #expect(store.notice == nil)
            let recovery = store.send(resolved(command))
            #expect(store.notice?.event == .failure(couldNotSendMessage("Rejected")))
            await clock.advance(by: .seconds(6))
            await recovery.finish()
        } else {
            #expect(store.notice?.event == .failure(couldNotSendMessage("Rejected")))
            await clock.advance(by: .seconds(6))
        }
        await operation.finish()
        #expect(store.notice == nil)
        // Duplicate and stale completions cannot create a second toast.
        for epoch in [UInt64.max, 0] {
            let stale: ServiceRootFeature.Action
            switch route {
            case .archive: stale = .tasks(.board(.archiveFinished(.rejected("Late"), epoch)))
            case .withdraw: stale = .tasks(.withdrawFinished("r-1", .rejected("Late")))
            default: stale = detailAction(.mutationFinished(.rejected("Late"), reopening ? .reopen : .cancel, epoch))
            }
            await store.send(stale).finish()
            #expect(store.notice == nil)
        }
        #expect(calls.value == [expected])
        observed.continuation.finish()
    }
}
