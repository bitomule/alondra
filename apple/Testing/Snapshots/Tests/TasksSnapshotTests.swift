import AlondraContract
import AlondraFeatures
import AlondraFeed
import AlondraiOSUI
import ComposableArchitecture
import SwiftUI
import XCTest

/// The Tasks tab and the task and request screens, from the daemon's own views and populated
/// samples: English and Spanish, light, dark and the largest accessibility text size.
@MainActor
final class TasksSnapshotTests: XCTestCase {
    private let lightAndDark: [SnapshotVariant] = [.light, .dark]

    private func tasks(_ document: ViewDocument, expanded: Set<String> = []) -> StoreOf<TasksFeature> {
        var state = TasksFeature.State()
        state.screen = .showing(document, key: .tasks)
        state.expanded = expanded
        return Store(initialState: state) { TasksFeature() } withDependencies: {
            $0.viewsClient.observe = ViewsClient.serving(document)
        }
    }

    private func tasksUnavailable(_ phase: ScreenFeature.State.Phase, observing: @escaping @Sendable (ViewKey) -> AsyncThrowingStream<ViewFetch, any Error>) -> StoreOf<TasksFeature> {
        var state = TasksFeature.State()
        state.screen.phase = phase
        return Store(initialState: state) { TasksFeature() } withDependencies: {
            $0.viewsClient.observe = observing
        }
    }

    private func task(_ document: ViewDocument) -> StoreOf<TaskDetailFeature> {
        let key = ViewKey(rawValue: document.key)
        var state = TaskDetailFeature.State(key: key)
        state.screen = .showing(document, key: key)
        return Store(initialState: state) { TaskDetailFeature() } withDependencies: {
            $0.viewsClient.observe = ViewsClient.serving(document)
        }
    }

    private func request(_ document: ViewDocument) -> StoreOf<RequestDetailFeature> {
        let key = ViewKey(rawValue: document.key)
        var state = RequestDetailFeature.State(key: key)
        state.screen = .showing(document, key: key)
        return Store(initialState: state) { RequestDetailFeature() } withDependencies: {
            $0.viewsClient.observe = ViewsClient.serving(document)
        }
    }

    func test_tasks_populated() throws {
        let document = try SampleViews.document("tasks-populated")
        assertScreen { ScreenFrame(height: 720) { TasksView(store: tasks(document)) } }
    }

    func test_tasks_populated_spanish() throws {
        let document = try SampleViews.document("tasks-populated-es")
        assertScreen(language: .spanish) { ScreenFrame(height: 720) { TasksView(store: tasks(document)) } }
    }

    func test_tasks_foldsOpen() throws {
        let document = try SampleViews.document("tasks-populated")
        assertScreen(lightAndDark) {
            ScreenFrame(height: 1000) { TasksView(store: tasks(document, expanded: ["finished", "archived"])) }
        }
    }

    func test_tasks_empty() throws {
        let document = try SampleViews.document("tasks-empty")
        assertScreen(lightAndDark) { ScreenFrame(height: 500) { TasksView(store: tasks(document)) } }
    }

    func test_tasks_empty_spanish() throws {
        let document = try SampleViews.document("tasks-empty-es")
        assertScreen(lightAndDark, language: .spanish) { ScreenFrame(height: 500) { TasksView(store: tasks(document)) } }
    }

    func test_tasks_failedToLoad() {
        let failure = String(localized: "Couldn’t load this from your Mac. Try again.")
        assertScreen(lightAndDark) {
            ScreenFrame(height: 400) {
                TasksView(store: tasksUnavailable(.failed(failure), observing: { _ in
                    AsyncThrowingStream { $0.finish(throwing: ViewsClientError.notInstalled) }
                }))
            }
        }
    }

    func test_tasks_theDaemonsOwnFixture() throws {
        let document = try SampleViews.document("tasks-ios")
        assertScreen(lightAndDark) { ScreenFrame(height: 500) { TasksView(store: tasks(document)) } }
    }

    func test_taskDetail() throws {
        let document = try SampleViews.document("task-detail")
        assertScreen { ScreenFrame(height: 1100) { TaskDetailView(store: task(document)) } }
    }

    func test_taskDetail_spanish() throws {
        let document = try SampleViews.document("task-detail-es")
        assertScreen(language: .spanish) { ScreenFrame(height: 1100) { TaskDetailView(store: task(document)) } }
    }

    func test_taskDetail_theDaemonsOwnFixture() throws {
        let document = try SampleViews.document("task-ios")
        assertScreen(lightAndDark) { ScreenFrame(height: 1100) { TaskDetailView(store: task(document)) } }
    }

    func test_taskDetail_gone() {
        var state = TaskDetailFeature.State(key: ViewKey(rawValue: "task:t-gone"))
        state.screen.phase = .missing
        let store = Store(initialState: state) { TaskDetailFeature() } withDependencies: {
            $0.viewsClient.observe = { _ in AsyncThrowingStream { $0.finish(throwing: ViewsClientError.missingView) } }
        }
        assertScreen(lightAndDark) { ScreenFrame(height: 320) { TaskDetailView(store: store) } }
    }

    func test_requestDetail() throws {
        let document = try SampleViews.document("request-detail")
        assertScreen { ScreenFrame(height: 700) { RequestDetailView(store: request(document)) } }
    }

    func test_requestDetail_spanish() throws {
        let document = try SampleViews.document("request-detail-es")
        assertScreen(language: .spanish) { ScreenFrame(height: 700) { RequestDetailView(store: request(document)) } }
    }
}
