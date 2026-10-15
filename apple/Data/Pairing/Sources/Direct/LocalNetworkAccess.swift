import Foundation

/// What this device knows about its Local Network permission. The system has no API to read
/// it, so it is learned from what happened: a browse that found something or a finished
/// pairing means allowed, a browse refused by policy means denied. Until then, nothing in
/// the app touches the local network, so the system never asks on its own.
public struct LocalNetworkAccess: Sendable {
    public enum State: String, Sendable, Equatable { case unknown, allowed, denied }

    /// Kept in the app's preferences, so it outlives the process.
    public static let standard = LocalNetworkAccess(
        load: { UserDefaults.standard.string(forKey: key).flatMap(State.init) },
        save: { UserDefaults.standard.set($0.rawValue, forKey: key) })

    private static let key = "alondra.localNetworkAccess"
    private let load: @Sendable () -> State?
    private let save: @Sendable (State) -> Void

    public init(load: @escaping @Sendable () -> State?, save: @escaping @Sendable (State) -> Void) {
        self.load = load
        self.save = save
    }

    public var state: State { load() ?? .unknown }

    public var allowed: Bool { state == .allowed }

    public func record(_ state: State) { save(state) }
}
