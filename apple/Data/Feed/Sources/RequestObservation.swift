import AlondraContract
import AlondraTransport
import Foundation

/// What `MonitoredOperations` saw of one request; the connection adds the Mac and the network.
public struct RequestObservation: Equatable, Sendable {
    public var operation: String
    public var method: String
    public var kind: String
    public var route: ConnectionRoute?
    public var duration: Duration
    public var outcome: ConnectionMeasurement.Outcome
    public var status: Int?
    public var error: String?

    public init(operation: String, method: String, kind: String, route: ConnectionRoute?, duration: Duration,
                outcome: ConnectionMeasurement.Outcome, status: Int? = nil, error: String? = nil) {
        self.operation = operation; self.method = method; self.kind = kind; self.route = route
        self.duration = duration; self.outcome = outcome; self.status = status; self.error = error
    }

    /// Only the route name: ids, queries and anything that is not a plain lowercase word are dropped.
    static func kind(of path: String) -> String {
        let first = path.drop(while: { $0 == "/" }).prefix(while: { $0 != "/" && $0 != "?" })
        guard !first.isEmpty, first.count <= 40, first.allSatisfy({ $0.isASCII && ($0.isLowercase || $0 == "-") }) else {
            return "other"
        }
        return String(first)
    }

    /// The error's type and case, never text a server or the system put in it.
    static func label(_ error: any Error) -> String {
        switch error {
        case let error as URLError: "URLError(\(error.code.rawValue))"
        case let error as RelayWireError: "RelayWireError.\(error)"
        case let error as OperationError: "OperationError.\(error)"
        case let error as DirectRouteError: "DirectRouteError.\(error)"
        case let error as DirectSessionError: "DirectSessionError.\(error)"
        case let error as SealedRequestError: "SealedRequestError.\(error)"
        case let error as IrohCarrierError: "IrohCarrierError.\(error)"
        case let error as APIError:
            switch error {
            case .unauthorized: "APIError.unauthorized"
            case .forbidden: "APIError.forbidden"
            case .conflict: "APIError.conflict"
            case .server(let status, _, _, _): "APIError.server(\(status))"
            case .unavailable: "APIError.unavailable"
            case .unexpected(let status): "APIError.unexpected(\(status))"
            }
        default: String(describing: type(of: error))
        }
    }
}
