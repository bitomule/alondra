import Foundation

/// What this device knows about its Local Network permission. The system has no API to read
/// it, so it is learned from what happened: a browse that found something or a finished
/// pairing means allowed, a browse refused by policy means denied. Until then, nothing in
/// the app touches the local network, so the system never asks on its own.
public enum LocalNetworkAccess {
    public enum State: String, Sendable, Equatable { case unknown, allowed, denied }

    private static let key = "alondra.localNetworkAccess"

    public static var state: State {
        UserDefaults.standard.string(forKey: key).flatMap(State.init) ?? .unknown
    }

    public static var allowed: Bool { state == .allowed }

    public static func record(_ state: State) {
        UserDefaults.standard.set(state.rawValue, forKey: key)
    }
}
