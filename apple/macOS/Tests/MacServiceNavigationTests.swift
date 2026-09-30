import AlondraViews
import AlondraViewsDependency
import AlondraWorkspaceFeature
import Foundation
import ComposableArchitecture
import Testing

@MainActor
struct MacServiceNavigationTests {
    @Test
    func changingSearchCancelsThePreviousViewsRequest() async throws {
        let clock = TestClock()
        let store = TestStore(initialState: MacPresentationFeature.State()) { MacPresentationFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.load = { key, _ in
                #expect(key.rawValue == "search?q=new+query")
                let data = Data(#"{"protocol":1,"view":"search","key":"search?q=new+query","client":"mac","version":"v0123456789abcdef","cursor":"c1.abc","data_epoch":"epoch","status":{"version":"s1","summary":"1 result","badge":1,"counts":[],"gauges":[],"orchestrator":{"text":"Ready","tone":"success"}},"sections":[{"id":"results","collapsed":false,"items":[{"type":"row","id":"new-node","icon":"node","title":"New node","depth":0,"struck":false,"children":[]}]}],"notices":[]}"#.utf8)
                return .document(try JSONDecoder().decode(ViewDocument.self, from: data))
            }
        }
        await store.send(.queryChanged("old query")) { $0.query = "old query"; $0.searchRevision = 1 }
        await store.send(.searchRequested)
        await clock.advance(by: .milliseconds(600))
        await store.send(.queryChanged("new query")) { $0.query = "new query"; $0.searchRevision = 2 }
        await store.send(.searchRequested)
        await clock.advance(by: .milliseconds(1200))
        await store.receive(\.searchStarted) { $0.smartStatus = String(localized: "Searching the conversations…") }
        await store.receive(\.searchFinished) {
            $0.smartMatches = ["new-node"]
            $0.smartStatus = String(localized: "Search results")
        }
    }

    @Test
    func sidebarQueryMatchesRowsAndExpandsProjectsWithoutReadingLegacyModels() async {
        let store = TestStore(initialState: MacPresentationFeature.State()) { MacPresentationFeature() }
        await store.send(.queryChanged("release checklist")) {
            $0.query = "release checklist"
            $0.searchRevision = 1
        }
        #expect(store.state.matches("Release checklist for Mac"))
        #expect(store.state.projectExpanded("Alondra"))
        await store.send(.queryChanged("   ")) { $0.query = "   "; $0.smartMatches = nil; $0.smartStatus = ""; $0.searchRevision = 2 }
        await store.send(.sidebarSectionExpanded("tasks", false)) { $0.tasksExpanded = false }
    }

    @Test
    func eachWindowKeepsItsOwnViewsSelection() {
        let first = Store(initialState: MacPresentationFeature.State()) { MacPresentationFeature() }
        let second = Store(initialState: MacPresentationFeature.State()) { MacPresentationFeature() }
        first.send(.select(.viewKey("pull_request:p2")))
        second.send(.select(.viewKey("node:worker")))
        #expect(first.selection == .viewKey("pull_request:p2"))
        #expect(second.selection == .viewKey("node:worker"))
    }
}
