import AlondraContract
import AlondraFeed
import AlondraFeatures
import Foundation
import ComposableArchitecture
import Testing

@MainActor
struct MacServiceNavigationTests {
    @Test
    func scheduleScreenOpensItsEditableFormWithServerDefaults() async throws {
        let data = Data(#"{"protocol":1,"view":"schedule","key":"schedule:new","client":"ios","version":"v1","cursor":"c1","data_epoch":"epoch","status":{"version":"s1","summary":"Ready","badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"sections":[{"id":"form","collapsed":false,"items":[{"type":"form","id":"schedule","fields":[{"id":"repeat","label":"Repeat","kind":{"kind":"choice","items":[{"id":"daily","label":"Daily"}]},"value":"daily","required":true,"reload_on_change":false},{"id":"time","label":"Time","kind":{"kind":"time"},"value":"09:00","required":true,"reload_on_change":false}],"submit":{"id":"sched_save:view@1","label":"Save Schedule","style":"primary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}}]}]}"#.utf8)
        let document = try JSONDecoder().decode(ViewDocument.self, from: data)
        let form = try #require(document.sections.first?.items.first.flatMap { item -> Form? in
            if case let .form(form) = item { form } else { nil }
        })
        let store = TestStore(initialState: ViewsScreenFeature.State(key: .newSchedule(), owner: "schedule-editor-test")) {
            ViewsScreenFeature()
        }

        await store.send(.loaded(.document(document))) {
            $0.document = document
            $0.action.dataEpoch = "epoch"
            $0.form = form
            $0.action.action = form.submit
            $0.action.fields = ["repeat": .string("daily"), "time": .string("09:00")]
            $0.action.requiresAttachments = false
        }
    }

    @Test
    func explicitRefreshOmitsTheKnownViewVersion() async throws {
        let data = Data(#"{"protocol":1,"view":"tasks","key":"tasks","client":"ios","version":"v1","cursor":"c1","data_epoch":"epoch","status":{"version":"s1","summary":"Ready","badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"sections":[]}"#.utf8)
        var state = ViewsScreenFeature.State(key: .tasks, owner: "refresh-test")
        state.document = try JSONDecoder().decode(ViewDocument.self, from: data)
        let store = TestStore(initialState: state) { ViewsScreenFeature() } withDependencies: {
            $0.viewsClient.openScreen = { _, _ in }
            $0.viewsClient.load = { key, known in
                #expect(key == .tasks)
                #expect(known == nil)
                return .unchanged(cursor: "c2")
            }
        }
        await store.send(.refresh) { $0.loading = true }
        await store.receive(\.loaded) { $0.loading = false }
    }

    @Test
    func fleetLoadFailureKeepsCachedDocumentForConnectionNotice() async throws {
        let data = Data(#"{"protocol":1,"view":"fleet","key":"fleet","client":"ios","version":"v1","cursor":"c1","data_epoch":"epoch","status":{"version":"s1","summary":"2 agents","badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"sections":[{"id":"agents","collapsed":false,"items":[]}] }"#.utf8)
        let document = try JSONDecoder().decode(ViewDocument.self, from: data)
        var state = ViewsScreenFeature.State(key: .fleet, owner: "fleet-offline-test")
        state.document = document
        state.isPresented = true
        let store = TestStore(initialState: state) { ViewsScreenFeature() }

        await store.send(.failed("offline")) { $0.error = "offline" }
        #expect(store.state.document == document)
    }

    private func searchDocument(empty: Bool) throws -> ViewDocument {
        let content = empty
            ? #""sections":[],"empty":{"text":"No results found","tone":"neutral"}"#
            : #""sections":[{"id":"results","collapsed":false,"items":[{"type":"row","id":"node-a","icon":"node","title":"Node A","depth":0,"struck":false,"children":[]}]}]"#
        let json = #"{"protocol":1,"view":"search","key":"search?q=node","client":"ios","version":"v1","cursor":"c1","data_epoch":"epoch","status":{"version":"s1","summary":"Ready","badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"# + content + "}"
        return try JSONDecoder().decode(ViewDocument.self, from: Data(json.utf8))
    }

    private func fleetSearchStore(
        clock: TestClock<Duration>,
        load: @escaping @Sendable (ViewKey, String?) async throws -> ViewFetch
    ) -> TestStoreOf<ViewsScreenFeature> {
        var state = ViewsScreenFeature.State(key: .fleet, owner: "fleet-search-test")
        state.isPresented = true
        return TestStore(initialState: state) { ViewsScreenFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.openScreen = { _, _ in }
            $0.viewsClient.load = load
        }
    }

    @Test
    func fleetSearchShowsTheMatches() async throws {
        let clock = TestClock()
        let document = try searchDocument(empty: false)
        let store = fleetSearchStore(clock: clock) { key, _ in
            #expect(key.rawValue == "search?q=node")
            return .document(document)
        }
        await store.send(.searchQueryChanged("node")) { $0.searchQuery = "node"; $0.searchLoading = true }
        await clock.advance(by: .milliseconds(250))
        await store.receive(\.searchLoaded) { $0.searchDocument = document; $0.searchLoading = false }
        #expect(store.state.searchError == nil)
    }

    @Test
    func fleetSearchReportsAnEmptyResult() async throws {
        let clock = TestClock()
        let document = try searchDocument(empty: true)
        let store = fleetSearchStore(clock: clock) { _, _ in .document(document) }
        await store.send(.searchQueryChanged("node")) { $0.searchQuery = "node"; $0.searchLoading = true }
        await clock.advance(by: .milliseconds(250))
        await store.receive(\.searchLoaded) { $0.searchDocument = document; $0.searchLoading = false }
        #expect(store.state.searchDocument?.empty?.text == "No results found")
        #expect(store.state.searchError == nil)
    }

    @Test
    func fleetSearchShowsAnErrorWhenItFails() async throws {
        struct Offline: Error {}
        let clock = TestClock()
        let store = fleetSearchStore(clock: clock) { _, _ in throw Offline() }
        await store.send(.searchQueryChanged("node")) { $0.searchQuery = "node"; $0.searchLoading = true }
        await clock.advance(by: .milliseconds(250))
        await store.receive(\.searchFailed) {
            $0.searchLoading = false
            $0.searchError = String(localized: "Couldn’t load this from your Mac. Try again.")
        }
        await store.send(.searchQueryChanged("nodes")) { $0.searchQuery = "nodes"; $0.searchLoading = true; $0.searchError = nil }
        await store.send(.searchQueryChanged("")) { $0.searchQuery = ""; $0.searchLoading = false }
    }

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
