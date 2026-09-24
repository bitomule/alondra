/// A node and the tick its detail last asked its threads at.
public struct ThreadsKey: Hashable, Sendable {
    public let session: String
    public let tick: UInt64
}
