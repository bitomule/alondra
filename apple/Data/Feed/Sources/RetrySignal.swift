import Synchronization

/// Wakes every answer waiting to go again: the network came back, the app came to the front, or
/// a push arrived. A wait that starts after a wake it has not seen returns at once, so no wake is
/// lost between a failed try and the wait that follows it.
final class RetrySignal: Sendable {
    private struct State {
        var generation = 0
        var next = 0
        var waiting: [Int: CheckedContinuation<Void, Never>] = [:]
    }
    private let state = Mutex(State())

    var generation: Int { state.withLock(\.generation) }

    func wait(since generation: Int) async {
        let id = state.withLock { state in
            state.next += 1
            return state.next
        }
        await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                let woken = state.withLock { state in
                    guard !Task.isCancelled, state.generation == generation else { return true }
                    state.waiting[id] = continuation
                    return false
                }
                if woken { continuation.resume() }
            }
        } onCancel: {
            state.withLock { $0.waiting.removeValue(forKey: id) }?.resume()
        }
    }

    func fire() {
        let woken = state.withLock { state in
            state.generation += 1
            defer { state.waiting.removeAll() }
            return Array(state.waiting.values)
        }
        for continuation in woken { continuation.resume() }
    }
}
