import Foundation
import Synchronization

/// Coloured code blocks, least recently used out first.
public final class CodeColourCache: Sendable {
    private struct Entry {
        let text: AttributedString
        var used: UInt64
    }

    private struct State {
        var entries: [CodeColourKey: Entry] = [:]
        var clock: UInt64 = 0
    }

    public let capacity: Int
    private let state = Mutex(State())

    public init(capacity: Int) {
        self.capacity = max(capacity, 1)
    }

    public var count: Int {
        state.withLock { $0.entries.count }
    }

    public subscript(key: CodeColourKey) -> AttributedString? {
        state.withLock { state in
            state.clock += 1
            state.entries[key]?.used = state.clock
            return state.entries[key]?.text
        }
    }

    public func store(_ text: AttributedString, for key: CodeColourKey) {
        state.withLock { state in
            state.clock += 1
            state.entries[key] = Entry(text: text, used: state.clock)
            if state.entries.count > capacity, let oldest = state.entries.min(by: { $0.value.used < $1.value.used })?.key {
                state.entries[oldest] = nil
            }
        }
    }
}
