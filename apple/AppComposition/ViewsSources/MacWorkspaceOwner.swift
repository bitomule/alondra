import AlondraLoopbackSession
import AlondraViewsWorkspaceServices
import Foundation
import Observation

/// Chooses where the Mac app's workspace comes from. The daemon on this Mac always wins; the
/// account's remote Mac is used only while no local daemon answers.
@MainActor
@Observable
public final class MacWorkspaceOwner {
    public enum Route: Equatable, Sendable { case searching, local, remote }

    public let account: CloudPairingOwner
    public private(set) var route: Route = .searching
    private var localServices: CloudAppServices?
    @ObservationIgnored private let isDaemonRunning: @Sendable () async -> Bool
    @ObservationIgnored private let openLocal: @MainActor () async throws -> CloudAppServices
    @ObservationIgnored private let startRemote: @MainActor () -> Void
    @ObservationIgnored private let reprobeInterval: Duration

    public convenience init(account: CloudPairingOwner, storageDirectory: URL, deviceName: String) {
        let identity = LoopbackIdentityStore(directory: storageDirectory.appendingPathComponent("loopback-identity", isDirectory: true))
        self.init(account: account,
                  isDaemonRunning: { await LoopbackSession.isDaemonRunning() },
                  openLocal: {
                      try await CloudAppServices.local(LoopbackSession(identity: identity, deviceName: deviceName),
                                                       identity: identity)
                  },
                  startRemote: { account.store.send(.appeared) })
    }

    public init(account: CloudPairingOwner, isDaemonRunning: @escaping @Sendable () async -> Bool,
         openLocal: @escaping @MainActor () async throws -> CloudAppServices,
         startRemote: @escaping @MainActor () -> Void, reprobeInterval: Duration = .seconds(10)) {
        self.account = account
        self.isDaemonRunning = isDaemonRunning
        self.openLocal = openLocal
        self.startRemote = startRemote
        self.reprobeInterval = reprobeInterval
    }

    public var services: CloudAppServices? {
        switch route {
        case .searching: nil
        case .local: localServices
        case .remote: account.services
        }
    }

    /// Runs for the window's lifetime. Keeps looking for a local daemon while on the remote
    /// route, so starting alondrad on this Mac takes over without a relaunch.
    public func run() async {
        while !Task.isCancelled, route != .local {
            if await isDaemonRunning(), let services = try? await openLocal() {
                guard !Task.isCancelled else { await services.deactivate(); return }
                localServices = services
                route = .local
                return
            }
            guard !Task.isCancelled else { return }
            if route == .searching {
                route = .remote
                startRemote()
            }
            do { try await Task.sleep(for: reprobeInterval) } catch { return }
        }
    }
}
