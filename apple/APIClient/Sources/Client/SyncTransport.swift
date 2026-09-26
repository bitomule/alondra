import Foundation

/// Supplies complete overview snapshots and a bounded change wait. A relay adapter may keep
/// its own revision/epoch cursor; the overview tick is only a local snapshot field.
public protocol SyncTransport: Sendable {
    func fetchOverview() async throws -> Data
    func waitForChange(afterSnapshotTick: UInt64, seconds: Int) async throws
}

extension AlondraAPIClient: SyncTransport {
    public func fetchOverview() async throws -> Data {
        try await send(AlondraEndpoints.overview())
    }

    public func waitForChange(afterSnapshotTick: UInt64, seconds: Int) async throws {
        try await operations.waitForChange(afterSnapshotTick: afterSnapshotTick, seconds: seconds)
    }
}
