import AlondraContract
import Synchronization

/// The cards whose answer this device holds and the Mac has not confirmed yet, so a queue drawn
/// after a relaunch keeps them out as answered.
public final class UndeliveredCards: Sendable {
    private struct State {
        var cards: Set<String> = []
        var listeners: [Int: AsyncStream<Set<String>>.Continuation] = [:]
        var next = 0
    }
    private let state = Mutex(State())

    public init() {}

    public var current: Set<String> { state.withLock(\.cards) }

    public func stream() -> AsyncStream<Set<String>> {
        let (stream, continuation) = AsyncStream<Set<String>>.makeStream(bufferingPolicy: .bufferingNewest(1))
        let id = state.withLock { state in
            state.next += 1
            state.listeners[state.next] = continuation
            continuation.yield(state.cards)
            return state.next
        }
        continuation.onTermination = { [weak self] _ in
            self?.state.withLock { _ = $0.listeners.removeValue(forKey: id) }
        }
        return stream
    }

    func update(_ cards: Set<String>) {
        let listeners = state.withLock { state -> [AsyncStream<Set<String>>.Continuation] in
            guard state.cards != cards else { return [] }
            state.cards = cards
            return Array(state.listeners.values)
        }
        for listener in listeners { listener.yield(cards) }
    }
}

public extension ActionRequest {
    /// The card an answer takes out of the queue: the subject of its action, `verb:subject@revision`.
    var answeredCard: String? {
        guard resolvesItsCard, let colon = actionId.firstIndex(of: ":"),
              let at = actionId[colon...].firstIndex(of: "@") else { return nil }
        let subject = actionId[actionId.index(after: colon)..<at]
        return subject.isEmpty ? nil : String(subject)
    }
}
