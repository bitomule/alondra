import AlondraPairing
import Foundation
import Testing

@MainActor struct SessionBoundActionTests {
    @Test func staleWindowCannotDispatchAfterSessionChangesBeforeActorEntry() async throws {
        let original = Session(), replacement = Session(), gate = Gate()
        let owner = Owner(original)
        // The scene passed its original preflight check, then waits for actor entry.
        #expect(owner.current === original)
        let action = Task {
            await gate.pause()
            do {
                try await SessionBoundAction.run(expected: original, current: { owner.current }) { _ in
                    owner.operations += 1 // Would authorize, contact a Mac and persist selection.
                }
                Issue.record("Stale window dispatched an operation")
            } catch {}
        }
        await gate.waitForPause()
        owner.current = replacement
        await gate.resume()
        await action.value
        #expect(owner.operations == 0)
        try await SessionBoundAction.run(expected: replacement, current: { owner.current }) { captured in
            #expect(captured === replacement)
            owner.operations += 1
        }
        #expect(owner.operations == 1)
        owner.current = nil
        do {
            try await SessionBoundAction.run(expected: replacement, current: { owner.current }) { _ in owner.operations += 1 }
            Issue.record("Logged-out window dispatched an operation")
        } catch {}
        #expect(owner.operations == 1)
    }
    private final class Session {}
    private final class Owner {
        var current: Session?
        var operations = 0
        init(_ session: Session) { current = session }
    }
    private actor Gate {
        var continuation: CheckedContinuation<Void, Never>?
        var observer: CheckedContinuation<Void, Never>?
        func pause() async { await withCheckedContinuation { continuation = $0; observer?.resume(); observer = nil } }
        func waitForPause() async { if continuation == nil { await withCheckedContinuation { observer = $0 } } }
        func resume() { continuation?.resume(); continuation = nil }
    }
}
