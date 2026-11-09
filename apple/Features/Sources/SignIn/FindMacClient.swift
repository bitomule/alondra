import ComposableArchitecture
import Foundation

/// A Mac advertising Alondra on this network, as the Find your Mac list shows it.
public struct FoundMac: Identifiable, Equatable, Sendable {
    public let id: String
    public let name: String
    public let paired: Bool
    public let macID: String?

    public init(id: String, name: String, paired: Bool, macID: String? = nil) {
        self.id = id
        self.name = name
        self.paired = paired
        self.macID = macID
    }
}

/// What both screens show while the Mac decides.
public struct DirectPairingCode: Equatable, Sendable {
    public let code: String
    public let macName: String

    public init(code: String, macName: String) {
        self.code = code
        self.macName = macName
    }
}

public enum FindMacTarget: Equatable, Sendable {
    case found(String)
    case address(String)
}

/// What the device knows about its Local Network permission.
public enum LocalNetworkPermission: Equatable, Sendable { case unknown, allowed, denied }

public enum FindMacUpdate: Equatable, Sendable {
    case macs([FoundMac])
    case denied
    /// Local Network access was allowed, Macs or not.
    case allowed
}

/// Pairing with a Mac on the same network, without an account.
public struct FindMacClient: Sendable {
    public var permission: @Sendable () -> LocalNetworkPermission
    public var browse: @Sendable () -> AsyncStream<FindMacUpdate>
    public var pair: @Sendable (FindMacTarget) async throws -> DirectPairingCode
    public var approve: @Sendable () async throws -> Void
    public var cancel: @Sendable () async -> Void
    /// A sentence the person can act on; never an error's type or raw description.
    public var describe: @Sendable (any Error) -> String
    /// Opens the system settings where Local Network access for Alondra is turned back on.
    public var openSettings: @Sendable () -> Void

    public init(permission: @escaping @Sendable () -> LocalNetworkPermission = { .allowed },
                browse: @escaping @Sendable () -> AsyncStream<FindMacUpdate>,
                pair: @escaping @Sendable (FindMacTarget) async throws -> DirectPairingCode,
                approve: @escaping @Sendable () async throws -> Void,
                cancel: @escaping @Sendable () async -> Void,
                describe: @escaping @Sendable (any Error) -> String,
                openSettings: @escaping @Sendable () -> Void = {}) {
        self.permission = permission
        self.browse = browse
        self.pair = pair
        self.approve = approve
        self.cancel = cancel
        self.describe = describe
        self.openSettings = openSettings
    }
}

private enum FindMacKey: DependencyKey {
    static let liveValue = FindMacClient(browse: { AsyncStream { $0.finish() } }, pair: { _ in throw CancellationError() },
        approve: { throw CancellationError() }, cancel: {},
        describe: { _ in String(localized: "Alondra couldn’t pair with that Mac. Try again.") })
}

public extension DependencyValues {
    var findMac: FindMacClient {
        get { self[FindMacKey.self] }
        set { self[FindMacKey.self] = newValue }
    }
}
