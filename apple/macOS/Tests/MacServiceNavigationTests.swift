import ComposableArchitecture
import Testing

@MainActor
struct MacServiceNavigationTests {
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
