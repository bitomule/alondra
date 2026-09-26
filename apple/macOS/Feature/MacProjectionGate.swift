/// Rejects a decoded response whose credential or connection generation is no longer current.
/// The window updates `expected` synchronously before the shared session switches identity.
public struct MacProjectionGate: Sendable {
    public private(set) var expected: MacSyncIdentity
    public private(set) var appliedGeneration: UInt64 = 0

    public init(expected: MacSyncIdentity) {
        self.expected = expected
    }

    public mutating func use(_ identity: MacSyncIdentity) {
        expected = identity
    }

    public mutating func accept(_ identity: MacSyncIdentity, generation: UInt64) -> Bool {
        guard identity == expected, generation >= appliedGeneration else { return false }
        appliedGeneration = generation
        return true
    }
}
