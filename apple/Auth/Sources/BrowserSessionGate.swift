import Foundation

/// Owns the one active web-auth continuation. Every completion and cancellation
/// carries the ID of the session that created it, so late work cannot touch its replacement.
@MainActor
public final class BrowserSessionGate {
    private struct Active {
        let id: UUID
        let cancel: @MainActor () -> Void
        let continuation: CheckedContinuation<URL, Error>
    }

    private var active: Active?
    private var newestGeneration = 0
    public init() {}
    public var isBusy: Bool { active != nil }
    public func isSuperseded(_ generation: Int) -> Bool { generation < newestGeneration }

    @discardableResult
    public func begin(id: UUID, generation: Int, cancel: @escaping @MainActor () -> Void,
                      continuation: CheckedContinuation<URL, Error>) -> Bool {
        guard generation >= newestGeneration else {
            continuation.resume(throwing: CancellationError())
            return false
        }
        guard active == nil else {
            continuation.resume(throwing: AuthError.invalidRedirect)
            return false
        }
        newestGeneration = generation
        active = Active(id: id, cancel: cancel, continuation: continuation)
        return true
    }

    public func finish(id: UUID, callback: URL?, error: Error?) {
        guard let current = active, current.id == id else { return }
        active = nil
        if let callback { current.continuation.resume(returning: callback) }
        else { current.continuation.resume(throwing: error ?? AuthError.invalidRedirect) }
    }

    public func cancel(id: UUID) {
        guard let current = active, current.id == id else { return }
        active = nil
        current.cancel()
        current.continuation.resume(throwing: CancellationError())
    }

    public func invalidate(generation: Int) {
        guard generation >= newestGeneration else { return }
        newestGeneration = generation
        if let id = active?.id { cancel(id: id) }
    }
}
