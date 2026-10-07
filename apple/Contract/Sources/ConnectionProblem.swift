import Foundation

/// Why the app cannot reach the fleet, in terms a person can act on. Every surface that shows a
/// connection failure reads this instead of an error's description.
public enum ConnectionProblem: Equatable, Sendable {
    /// The device itself has no network.
    case noInternet
    /// The relay server does not answer.
    case serverUnreachable
    /// The relay answers but the Mac never replies: it is off, or Alondra is not running on it.
    /// The relay cannot tell these apart.
    case macNotAnswering
    /// Nothing answers on this Mac's loopback port.
    case alondraNotRunning
    /// The account session ended; signing in again restores it. Only the sign-in server rejecting
    /// a refresh (invalid_grant) ends it.
    case sessionExpired
    /// The sign-in server or the relay refused this device's access without ending the session,
    /// as Keycloak does right after a restart. The session is kept and Alondra retries.
    case accessNotConfirmed
    /// The Mac or relay no longer accepts this device; it has to be paired again. `macName` is the
    /// paired Mac's name when it is known.
    case deviceRevoked(macName: String?)
    /// A Mac paired without an account answers on none of its direct routes from this network.
    case macNotOnNetwork
    /// Like `macNotOnNetwork`, and an address that answered as this Mac before now answers as the
    /// same Mac with another identity. Only a hint: nothing is trusted or replaced until the person
    /// pairs again.
    case macChangedIdentity
    /// An earlier sign-out could not reach the server; it has to finish before signing in again.
    case signOutIncomplete
    case unexpected

    /// Problems only the person can fix. Automatic retries continue, but they cannot succeed.
    public var isDeviceRevoked: Bool {
        if case .deviceRevoked = self { true } else { false }
    }

    public var needsAction: Bool {
        switch self {
        case .sessionExpired, .deviceRevoked, .signOutIncomplete: true
        default: false
        }
    }
}

/// Exponential backoff with equal jitter: the ceiling doubles from 1 s up to 60 s, and each wait
/// is between half the ceiling and the ceiling so many clients do not retry in lockstep.
public struct ConnectionBackoff: Sendable {
    public static let standard = ConnectionBackoff()

    public let initial: Duration
    public let maximum: Duration

    public init(initial: Duration = .seconds(1), maximum: Duration = .seconds(60)) {
        self.initial = initial
        self.maximum = maximum
    }

    /// `attempt` counts consecutive failures, starting at 0. `random` is in 0...1.
    public func delay(attempt: Int, random: Double) -> Duration {
        let ceiling = min(maximum, initial * Double(1 << min(max(attempt, 0), 30)))
        let half = ceiling / 2
        return half + half * min(max(random, 0), 1)
    }
}
