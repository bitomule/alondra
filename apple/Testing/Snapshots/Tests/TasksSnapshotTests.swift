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

    func test_tasks_populated() throws {
        let document = try SampleViews.document("tasks-populated")
        assertScreen { ScreenFrame(height: 1000) { TasksView(store: tasks(document)) } }
    }

    func test_tasks_populated_spanish() throws {
        let document = try SampleViews.document("tasks-populated-es")
        assertScreen(language: .spanish) { ScreenFrame(height: 1000) { TasksView(store: tasks(document)) } }
    }

    func test_tasks_foldsOpen() throws {
        let document = try SampleViews.document("tasks-populated")
        assertScreen(lightAndDark) {
            ScreenFrame(height: 1400) { TasksView(store: tasks(document, expanded: ["finished", "archived"])) }
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

    private func daemonsOwnTasks(_ document: ViewDocument, expanded: Set<String> = [], height: CGFloat) -> some View {
        ScreenFrame(height: height) {
            TasksView(store: tasks(document, expanded: expanded)).environment(\.referenceDate, screenReferenceDate)
        }
    }

    func test_tasks_theDaemonsOwnFixture() throws {
        let document = try SampleViews.document("tasks-ios")
        assertScreen(lightAndDark, inWindow: true) { daemonsOwnTasks(document, height: 1100) }
    }

    func test_tasks_theDaemonsOwnFixture_spanish() throws {
        let document = try SampleViews.document("tasks-ios-es")
        assertScreen(lightAndDark, language: .spanish, inWindow: true) { daemonsOwnTasks(document, height: 1200) }
    }

    func test_tasks_theDaemonsOwnFixture_foldsOpen() throws {
        let document = try SampleViews.document("tasks-ios")
        assertScreen(lightAndDark, inWindow: true) {
            daemonsOwnTasks(document, expanded: ["finished"], height: 1300)
        }
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

    func test_taskDetail_nodeClosed_offersRelaunch() throws {
        let document = try SampleViews.document("task-closed-ios")
        assertScreen([.light], inWindow: true) { ScreenFrame(height: 1000) { TaskDetailView(store: task(document)) } }
    }

    func test_taskDetail_nodeClosed_offersRelaunch_spanish() throws {
        let document = try SampleViews.document("task-closed-ios-es")
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 1000) { TaskDetailView(store: task(document)) } }
    }

    func test_taskDetail_gone() {
        var state = TaskDetailFeature.State(key: ViewKey(rawValue: "task:t-gone"))
        state.screen.phase = .missing
        let store = Store(initialState: state) { TaskDetailFeature() } withDependencies: {
            $0.viewsClient.observe = { _ in AsyncThrowingStream { $0.finish(throwing: ViewsClientError.missingView) } }
        }
        assertScreen(lightAndDark) { ScreenFrame(height: 320) { TaskDetailView(store: store) } }
    }
}
