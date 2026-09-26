import AlondraWorkspaceFeature
import AlondraShared
import Foundation
import ComposableArchitecture
import Testing

@MainActor
struct MacServiceNavigationTests {
    @Test
    func changingSearchCancelsTheConversationFallbackBeforeDispatch() async throws {
        let clock = TestClock()
        let store = TestStore(initialState: MacPresentationFeature.State()) { MacPresentationFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.macConversationSearch = MacConversationSearch(run: { query in
                #expect(query == "new query")
                return try JSONDecoder().decode(SearchResult.self, from: Data(#"{"via":"conversation","sessions":["new-node"]}"#.utf8))
            })
        }
        await store.send(.queryChanged("old query")) { $0.query = "old query"; $0.searchRevision = 1 }
        await store.send(.searchRequested(inbox: [], board: .empty, fleet: nil))
        await clock.advance(by: .milliseconds(600))
        await store.send(.queryChanged("new query")) { $0.query = "new query"; $0.searchRevision = 2 }
        await store.send(.searchRequested(inbox: [], board: .empty, fleet: nil))
        await clock.advance(by: .milliseconds(1200))
        await store.receive(\.searchStarted) { $0.smartStatus = String(localized: "Searching the conversations…") }
        await store.receive(\.searchFinished) {
            $0.smartMatches = ["new-node"]
            $0.smartStatus = String(localized: "Found in the conversations")
        }
    }

    @Test
    func resolvingMiddleCardFollowsItsNeighbourWithoutStealingAnotherWindowsSelection() {
        let first = Store(initialState: MacPresentationFeature.State()) { MacPresentationFeature() }
        let second = Store(initialState: MacPresentationFeature.State()) { MacPresentationFeature() }
        for store in [first, second] { store.send(.inboxChanged(["q1", "q2", "q3"])) }
        first.send(.select(.item("q2")))
        second.send(.select(.node("worker")))

        for store in [first, second] { store.send(.inboxChanged(["q1", "q3"])) }
        #expect(first.selection == .item("q3"))
        #expect(second.selection == .node("worker"))

        first.send(.inboxChanged(["q1"]))
        #expect(first.selection == .item("q1"))
        first.send(.inboxChanged([]))
        #expect(first.selection == nil)
        first.send(.inboxChanged(["q4"]))
        #expect(first.selection == nil, "Once triage ends, arriving work must not steal focus from the home screen")
    }
}
