import Foundation
import Synchronization

struct DeadlineExceeded: Error, Equatable {}

/// Runs work that may not stop when cancelled (a call into Rust) and gives up on it at a
/// deadline or when the caller is cancelled, leaving the work to finish on its own.
enum Deadline {
    static func run<T: Sendable>(within timeout: Duration,
                                 _ operation: @escaping @Sendable () async throws -> T) async throws -> T {
        let outcome = Outcome<T>()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                guard outcome.wait(continuation) else { return }
                let work = Task {
                    do { outcome.finish(.success(try await operation())) }
                    catch { outcome.finish(.failure(error)) }
                }
                let timer = Task {
                    try? await Task.sleep(for: timeout)
                    outcome.finish(.failure(DeadlineExceeded()))
                }
                outcome.onFinish { work.cancel(); timer.cancel() }
            }
        } onCancel: {
            outcome.finish(.failure(CancellationError()))
        }
    }
}

private final class Outcome<T: Sendable>: Sendable {
    private enum State {
        case pending(CheckedContinuation<T, any Error>?, cleanup: (@Sendable () -> Void)?)
        case finished(Result<T, any Error>?)
    }

    private let state = Mutex(State.pending(nil, cleanup: nil))

    /// False when the outcome is already decided (the caller was cancelled first).
    func wait(_ continuation: CheckedContinuation<T, any Error>) -> Bool {
        let early: Result<T, any Error>? = state.withLock { state in
            switch state {
            case .pending(_, let cleanup):
                state = .pending(continuation, cleanup: cleanup)
                return nil
            case .finished(let result):
                state = .finished(nil)
                return result
            }
        }
        if let early { continuation.resume(with: early) }
        return early == nil
    }

    func onFinish(_ cleanup: @escaping @Sendable () -> Void) {
        let finished = state.withLock { state in
            guard case .pending(let continuation, _) = state else { return true }
            state = .pending(continuation, cleanup: cleanup)
            return false
        }
        if finished { cleanup() }
    }

    func finish(_ result: Result<T, any Error>) {
        let pending: (CheckedContinuation<T, any Error>?, (@Sendable () -> Void)?)? = state.withLock { state in
            guard case .pending(let continuation, let cleanup) = state else { return nil }
            state = .finished(continuation == nil ? result : nil)
            return (continuation, cleanup)
        }
        guard let pending else { return }
        pending.0?.resume(with: result)
        pending.1?()
    }
}
