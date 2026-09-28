import AlondraAPIClient
import AlondraAuth
import AlondraContracts
import AlondraLoopbackSession
import AlondraRelaySession
import Foundation

/// The one place that turns a transport, relay or account error into what the person sees.
public enum ConnectionClassifier {
    public static func problem(for error: any Error, local: Bool, networkOffline: Bool) -> ConnectionProblem {
        local ? loopbackProblem(error) : remoteProblem(error, networkOffline: networkOffline)
    }

    private static func loopbackProblem(_ error: any Error) -> ConnectionProblem {
        if let error = error as? URLError,
           [.cannotConnectToHost, .cannotFindHost, .networkConnectionLost, .timedOut].contains(error.code) {
            return .alondraNotRunning
        }
        if case .status = error as? LoopbackSessionError { return .alondraNotRunning }
        return .unexpected
    }

    private static func remoteProblem(_ error: any Error, networkOffline: Bool) -> ConnectionProblem {
        if networkOffline { return .noInternet }
        switch error {
        case let error as URLError: return urlProblem(error)
        case let error as AuthError: return authProblem(error)
        case let error as RelayWireError:
            switch error {
            case .unauthorized: return .deviceRevoked
            case .outcomeUnknown: return .macNotAnswering
            case .server(403): return .sessionExpired
            case .server(let status) where status >= 500: return .serverUnreachable
            default: return .unexpected
            }
        case let error as RelaySessionError:
            switch error {
            case .outcomeUnknown: return .macNotAnswering
            case .localStatus(401), .localStatus(403): return .deviceRevoked
            default: return .unexpected
            }
        case ServiceAPIError.outcomeUnknown: return .macNotAnswering
        case APIError.unauthorized: return .deviceRevoked
        default: return .unexpected
        }
    }

    private static func urlProblem(_ error: URLError) -> ConnectionProblem {
        switch error.code {
        case .notConnectedToInternet, .dataNotAllowed, .internationalRoamingOff: .noInternet
        case .cannotFindHost, .cannotConnectToHost, .dnsLookupFailed, .timedOut, .networkConnectionLost,
             .secureConnectionFailed, .badServerResponse, .cannotLoadFromNetwork: .serverUnreachable
        case .userAuthenticationRequired: .sessionExpired
        default: .unexpected
        }
    }

    private static func authProblem(_ error: AuthError) -> ConnectionProblem {
        switch error {
        case .noSession, .invalidToken, .pendingRevocation, .rotationUncertain: .sessionExpired
        case .oauth(let status, _) where status >= 500: .serverUnreachable
        case .oauth: .sessionExpired
        case .httpStatus(let status) where status >= 500: .serverUnreachable
        case .httpStatus(401), .httpStatus(403): .sessionExpired
        default: .unexpected
        }
    }
}
