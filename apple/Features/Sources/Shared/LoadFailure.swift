import AlondraTransport
import Foundation

/// Why a screen could not load, as far as the app can tell. Everything but `refused` usually
/// clears on its own, so a screen keeps trying and only says so once it keeps failing.
public enum LoadFailure: Equatable, Sendable {
    /// The request reached the relay or the Mac, and no answer came back in time.
    case macSilent
    /// The network between this device and the Alondra server failed the request.
    case connectionDropped
    /// The Alondra server turned the request away for now.
    case serverBusy
    /// An answer that retrying does not change.
    case refused

    public init(_ error: any Error) {
        switch error {
        case let error as URLError where error.code != .cancelled:
            self = .connectionDropped
        case RelayWireError.admissionFull, RelayWireError.server(429), RelayWireError.server(503):
            self = .serverBusy
        case let RelayWireError.server(status) where (500...599).contains(status):
            self = .connectionDropped
        case OperationError.outcomeUnknown, RelayWireError.outcomeUnknown, RelaySessionError.outcomeUnknown,
             DirectRouteError.unreachable, APIError.unavailable:
            self = .macSilent
        default:
            self = .refused
        }
    }

    public var isPassing: Bool { self != .refused }

    public var message: String {
        switch self {
        case .macSilent: String(localized: "Your Mac didn’t answer in time. Trying again…")
        case .connectionDropped: String(localized: "The connection to the Alondra server dropped. Trying again…")
        case .serverBusy: String(localized: "The Alondra server is busy. Trying again…")
        case .refused: String(localized: "Couldn’t load this from your Mac. Try again.")
        }
    }
}
