import AlondraFeatures
import AlondraTransport
import Foundation
import Observation

/// Chooses where the Mac app's workspace comes from. The daemon on this Mac wins when it answers;
/// other Macs paired on the local network or with the account stay open next to it, and the
/// person switches between them in Settings.
@MainActor
@Observable
public final class MacWorkspaceOwner {
    public enum Route: Equatable, Sendable { case searching, local, remote }

    public let account: AccountSession
    public private(set) var route: Route = .searching
    /// On the local route: whether the window shows this Mac or the account's selected Mac.
    public private(set) var showsThisMac = true
    public let thisMacChoiceID = UUID()
    private var localServices: Connection?
    @ObservationIgnored private let isDaemonRunning: @Sendable () async -> Bool
    @ObservationIgnored private let openLocal: @MainActor () async throws -> Connection
    @ObservationIgnored private let startRemote: @MainActor () -> Void
    @ObservationIgnored private let reprobeInterval: Duration
    @ObservationIgnored private let ownMacID: @Sendable () async -> String?

    public convenience init(account: AccountSession, storageDirectory: URL, deviceName: String) {
        let identity = LoopbackIdentityStore(directory: storageDirectory.appendingPathComponent("loopback-identity", isDirectory: true))
        self.init(account: account,
                  isDaemonRunning: { await LoopbackSession.isDaemonRunning() },
                  openLocal: {
                      try await Connection.local(LoopbackSession(identity: identity, deviceName: deviceName),
                                                       identity: identity, storageDirectory: storageDirectory)
                  },
                  startRemote: { account.store.send(.appeared) },
                  ownMacID: { await LoopbackSession.ownMacID() })
    }

    public init(account: AccountSession, isDaemonRunning: @escaping @Sendable () async -> Bool,
         openLocal: @escaping @MainActor () async throws -> Connection,
         startRemote: @escaping @MainActor () -> Void, reprobeInterval: Duration = .seconds(10),
         ownMacID: @escaping @Sendable () async -> String? = { nil }) {
        self.ownMacID = ownMacID
        self.account = account
        self.isDaemonRunning = isDaemonRunning
        self.openLocal = openLocal
        self.startRemote = startRemote
        self.reprobeInterval = reprobeInterval
    }

    public var services: Connection? {
        switch route {
        case .searching: nil
        case .local: showsThisMac || account.services == nil ? localServices : account.services
        case .remote: account.services
        }
    }

    /// This Mac's own daemon, whichever Mac the window shows.
    public var thisMac: Connection? { localServices }

    /// The Settings list on the local route: this Mac first, then every paired Mac.
    public var macChoices: [MacChoice] {
        guard route == .local else { return account.macs }
        return [MacChoice(id: thisMacChoiceID, macID: "this-mac", name: String(localized: "This Mac"))] + account.macs
    }

    public var selectedChoiceID: UUID? {
        route == .local && showsThisMac ? thisMacChoiceID : account.selectedMacID
    }

    public func select(choice id: UUID) async throws {
        if id == thisMacChoiceID {
            guard route == .local else { throw CancellationError() }
            showsThisMac = true
            return
        }
        if id != account.selectedMacID {
            guard let current = account.services else { throw CancellationError() }
            try await account.selectMac(choice: id, expectedSession: current)
        }
        showsThisMac = false
    }

    /// Runs for the window's lifetime. Keeps looking for a local daemon while on the remote
    /// route, so starting alondrad on this Mac takes over without a relaunch. The saved Macs are
    /// restored either way.
    public func run() async {
        var restoredPaired = false
        while !Task.isCancelled, route != .local {
            if await isDaemonRunning(), let services = try? await openLocal() {
                guard !Task.isCancelled else { await services.deactivate(); return }
                localServices = services
                route = .local
                account.ownMacID = await ownMacID()
                if !restoredPaired { startRemote() }
                return
            }
            guard !Task.isCancelled else { return }
            if route == .searching {
                route = .remote
                restoredPaired = true
                startRemote()
            }
            do { try await Task.sleep(for: reprobeInterval) } catch { return }
        }
    }
}
