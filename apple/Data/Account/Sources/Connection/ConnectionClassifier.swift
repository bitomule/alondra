import AlondraTransport
import AlondraContract
import AlondraFeatures
import AlondraFeed
import Foundation

/// The one place that turns a transport, relay or account error into what the person sees.
public enum ConnectionClassifier {
    public static func problem(for error: any Error, local: Bool, networkOffline: Bool,
                               macName: String? = nil) -> ConnectionProblem {
        // Checked before the network: a store on this device failing says nothing about the Mac.
        if error is ViewActionIntentError { return .deviceStorage }
        let problem = local ? loopbackProblem(error) : remoteProblem(error, networkOffline: networkOffline, macName: macName)
        if problem == .accessNotConfirmed || problem == .sessionExpired {
            AccountDiagnostics.personSees(problem, after: label(error))
        }
        return problem
    }

    /// The error's type and case, without any server-supplied text.
    private static func label(_ error: any Error) -> String {
        switch error {
        case AuthError.oauth(let status, _): "AuthError.oauth(status: \(status))"
        case let error as AuthError: "AuthError.\(error)"
        case let error as RelayWireError: "RelayWireError.\(error)"
        case let error as URLError: "URLError(\(error.code.rawValue))"
        default: String(describing: type(of: error))
        }
    }

    private static func loopbackProblem(_ error: any Error) -> ConnectionProblem {
        if let error = error as? URLError,
           [.cannotConnectToHost, .cannotFindHost, .networkConnectionLost, .timedOut].contains(error.code) {
            return .alondraNotRunning
        }
        if case .status = error as? LoopbackSessionError { return .alondraNotRunning }
        return .unexpected
    }

    private static func remoteProblem(_ error: any Error, networkOffline: Bool, macName: String?) -> ConnectionProblem {
        // The Mac's own sealed answer reached this device, so the network is not the problem.
        if error as? DirectSessionError == .deviceRevoked { return .deviceRevoked(macName: macName) }
        if networkOffline { return .noInternet }
        switch error {
        case DirectRouteError.unreachable: return .macNotOnNetwork
        case DirectRouteError.identityChanged: return .macChangedIdentity
        case DirectRouteError.localNetworkDenied: return .localNetworkDenied
        case let error as URLError: return urlProblem(error)
        case let error as AuthError: return authProblem(error)
        case let error as RelayWireError:
            switch error {
            // Unsealed: only the Mac's sealed device_revoked answer proves a revocation.
            case .unauthorized, .server(403): return .accessNotConfirmed
            case .outcomeUnknown: return .macNotAnswering
            // The relay is up and holding requests the Mac is not taking: its queue is full.
            case .admissionFull, .server(429): return .macNotAnswering
            case .server(let status) where status >= 500: return .serverUnreachable
            default: return .unexpected
            }
        case let error as RelaySessionError:
            switch error {
            case .outcomeUnknown: return .macNotAnswering
            default: return .unexpected
            }
        case OperationError.outcomeUnknown: return .macNotAnswering
        case APIError.unauthorized: return .accessNotConfirmed
        default: return .unexpected
        }
    }

    private static func urlProblem(_ error: URLError) -> ConnectionProblem {
        switch error.code {
        case .notConnectedToInternet, .dataNotAllowed, .internationalRoamingOff: .noInternet
        case .cannotFindHost, .cannotConnectToHost, .dnsLookupFailed, .timedOut, .networkConnectionLost,
             .secureConnectionFailed, .badServerResponse, .cannotLoadFromNetwork: .serverUnreachable
        case .userAuthenticationRequired: .accessNotConfirmed
        default: .unexpected
        }
    }

    private static func authProblem(_ error: AuthError) -> ConnectionProblem {
        switch error {
        case .noSession, .unreadableCredential, .pendingRevocation, .rotationUncertain: .sessionExpired
        case .invalidToken: .accessNotConfirmed
        case .oauth(let status, _) where status >= 500: .serverUnreachable
        case .oauth(_, "invalid_grant"): .sessionExpired
        case .oauth: .accessNotConfirmed
        case .httpStatus(let status) where status >= 500: .serverUnreachable
        case .httpStatus(401), .httpStatus(403): .accessNotConfirmed
        default: .unexpected
        }
    }
}
