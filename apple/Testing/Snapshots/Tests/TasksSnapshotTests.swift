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

    /// `opened` are the pages the daemon sends for a fold when it is opened.
    private func tasks(_ document: ViewDocument, expanded: Set<String> = [], opened: [ViewDocument] = [],
                       openChains: Set<String> = []) -> StoreOf<TasksFeature> {
        var state = TasksFeature.State()
        state.screen = .showing(document, key: .tasks)
        state.expanded = expanded
        state.openChains = openChains
        for page in opened {
            for section in page.sections where expanded.contains(section.id) {
                let rows = section.items.compactMap { if case .row(let row) = $0 { row } else { nil } }
                state.pages[section.id] = TasksPage(rows: rows, more: section.more)
            }
        }
        state.pagesVersion = opened.isEmpty ? nil : document.version
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

    func test_tasks_theMacNotAnsweringWithSavedTasks() throws {
        let document = try SampleViews.document("tasks-empty")
        assertScreen(lightAndDark) {
            ScreenFrame(height: 500) { TasksView(store: tasks(document)) }
                .environment(\.connectionState, .unreachable(.macNotAnswering))
        }
    }

    func test_tasks_theMacNotAnsweringWithSavedTasks_spanish() throws {
        let document = try SampleViews.document("tasks-empty-es")
        assertScreen(language: .spanish) {
            ScreenFrame(height: 500) { TasksView(store: tasks(document)) }
                .environment(\.connectionState, .unreachable(.macNotAnswering))
        }
    }

    func test_tasks_nothingSavedAndTheMacNotAnswering_spanish() {
        assertScreen(language: .spanish) {
            ScreenFrame(height: 400) {
                TasksView(store: tasksUnavailable(.loading, observing: { _ in AsyncThrowingStream { _ in } }))
            }
            .environment(\.connectionState, .unreachable(.macNotAnswering))
        }
    }

    private func daemonsOwnTasks(_ document: ViewDocument, expanded: Set<String> = [], opened: [ViewDocument] = [],
                                 openChains: Set<String> = [], height: CGFloat) -> some View {
        ScreenFrame(height: height) {
            TasksView(store: tasks(document, expanded: expanded, opened: opened, openChains: openChains))
                .environment(\.referenceDate, screenReferenceDate)
        }
    }

    func test_tasks_theDaemonsOwnFixture() throws {
        let document = try SampleViews.document("tasks-ios")
        assertScreen(lightAndDark, inWindow: true) { daemonsOwnTasks(document, height: 1650) }
    }

    func test_tasks_theDaemonsOwnFixture_spanish() throws {
        let document = try SampleViews.document("tasks-ios-es")
        assertScreen(lightAndDark, language: .spanish, inWindow: true) { daemonsOwnTasks(document, height: 1750) }
    }

    /// A task whose node waits for its own command is working, with no command shown; one whose node the machine
    /// paused says it is paused and why, never that it works.
    func test_tasks_waitingForItsCommandAndPausedByTheMachine_spanish() throws {
        let document = try ScreenFixtures.document("node-states-tasks-ios-es")
        assertScreen(lightAndDark, language: .spanish, inWindow: true) { daemonsOwnTasks(document, height: 1750) }
    }

    func test_tasks_theDaemonsOwnFixture_chainOpen_spanish() throws {
        let document = try SampleViews.document("tasks-ios-es")
        assertScreen(lightAndDark, language: .spanish, inWindow: true) {
            daemonsOwnTasks(document, openChains: ["chain:t-651c38484cd2f34e74d5107ba05f7f15"], height: 2050)
        }
    }

    func test_tasks_theDaemonsOwnFixture_foldsOpen() throws {
        let document = try SampleViews.document("tasks-ios")
        let finished = try SampleViews.document("tasks-ios-finished")
        assertScreen(lightAndDark, inWindow: true) {
            daemonsOwnTasks(document, expanded: ["finished"], opened: [finished], height: 2000)
        }
    }

    func test_tasks_theDaemonsOwnFixture_foldsOpen_spanish() throws {
        let document = try SampleViews.document("tasks-ios-es")
        let finished = try SampleViews.document("tasks-ios-es-finished")
        assertScreen(lightAndDark, language: .spanish, inWindow: true) {
            daemonsOwnTasks(document, expanded: ["finished"], opened: [finished], height: 2100)
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

    func test_taskDetail_queuedWithItsPriority() throws {
        let document = try ScreenFixtures.document("task-pacing-ios")
        assertScreen([.light], inWindow: true) { ScreenFrame(height: 1300) { TaskDetailView(store: task(document)) } }
        let spanish = try ScreenFixtures.document("task-pacing-ios-es")
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 1300) { TaskDetailView(store: task(spanish)) } }
    }

    func test_taskDetail_working() throws {
        let document = try SampleViews.document("task-detail-working")
        assertScreen(lightAndDark, inWindow: true) { ScreenFrame(height: 2500) { TaskDetailView(store: task(document)) } }
    }

    func test_taskDetail_working_spanish() throws {
        let document = try SampleViews.document("task-detail-working-es")
        assertScreen(lightAndDark, language: .spanish, inWindow: true) { ScreenFrame(height: 2500) { TaskDetailView(store: task(document)) } }
    }

    func test_taskDetail_theDaemonsOwnFixture() throws {
        let document = try SampleViews.document("task-ios")
        assertScreen(lightAndDark) { ScreenFrame(height: 1400) { TaskDetailView(store: task(document)) } }
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
