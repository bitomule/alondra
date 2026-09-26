import ComposableArchitecture
import Foundation
import Testing

@MainActor
@Test func tcaFailuresReachSwiftTesting() async {
    let check = {
        let store = TestStore(initialState: 0) {
            Reduce<Int, Bool> { state, _ in
                state += 1
                return .none
            }
        }
        // Deliberately omit the state assertion: TestStore must report a failure.
        await store.send(true)
    }
    if ProcessInfo.processInfo.environment["ALONDRA_REPORTER_NEGATIVE_CONTROL"] == "1" {
        await check()
    } else {
        await withKnownIssue("The intentional TestStore mismatch must reach Swift Testing") {
            await check()
        }
    }
}
