import Foundation

/// Distinguishes this invocation from an older operation that prevented it from dispatching.
/// A resolution for the older UUID must never acknowledge the new caller's draft.
actor CommandDispatchWitness {
    private(set) var invoked = false
    func markInvoked() { invoked = true }
}
struct PreviousCommandStillPending: LocalizedError {
    var errorDescription: String? { "An earlier operation is still being checked. This draft was not sent. Try again after its result is known." }
}
