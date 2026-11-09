import Foundation

/// The stage and machine-readable reason survive all the way to the settings screen.
public struct RegistrationFailure: Error, Equatable, Sendable {
    public enum Operation: Equatable, Sendable { case relay, mac, activityKey }
    public enum Reason: Equatable, Sendable {
        case unavailable, authenticationRequired, approvalRequired, notEnrolled
        case identityChanged, localNetworkDenied, accountDeviceRemoved
        case stalePreferences(UInt64), capacity, expired, incompatible
        case invalidRequest(Rejection), rejected, local
    }
    public enum Rejection: String, Decodable, Sendable {
        case invalidBody = "invalid_registration_body"
        case unknownField = "unknown_registration_field"
        case invalidBinding = "invalid_push_binding"
        case invalidQuietHours = "invalid_quiet_hours"
        case invalidRevision = "invalid_preferences_revision"
        case missingQuietHours = "missing_quiet_hours"
        case invalidActivityKey = "invalid_activity_key"
        case notEnrolled = "device_not_enrolled"
        case stalePreferences = "stale_push_preferences"
        case unsupportedRoute = "unsupported_registration_route"
        case invalidMethod = "invalid_registration_method"
        case approvalRevoked = "device_approval_revoked"
        case capacity = "operation_capacity"
        case expired = "registration_expired"

        var status: Int {
            switch self {
            case .notEnrolled, .stalePreferences: 409
            case .unsupportedRoute: 404
            case .invalidMethod: 400
            case .approvalRevoked: 403
            case .capacity: 429
            case .expired: 410
            default: 422
            }
        }
    }

    public let operation: Operation
    public let reason: Reason
    public init(operation: Operation, reason: Reason) { self.operation = operation; self.reason = reason }

    public var retries: Bool {
        switch reason {
        case .unavailable, .notEnrolled, .stalePreferences, .capacity, .expired: true
        default: false
        }
    }

    public var message: String {
        let subject: String = switch operation {
        case .relay: String(localized: "Alondra’s relay")
        case .mac: String(localized: "Alerts on your Mac")
        case .activityKey: String(localized: "Live Activity key on your Mac")
        }
        let action: String = switch reason {
        case .unavailable: String(localized: "Connection unavailable. Retrying automatically…")
        case .notEnrolled: String(localized: "Waiting for your Mac to finish linking with the relay. Retrying automatically…")
        case .stalePreferences: String(localized: "Updating notification preferences. Retrying automatically…")
        case .capacity, .expired: String(localized: "Registration is temporarily delayed. Retrying automatically…")
        case .authenticationRequired: String(localized: "Sign in again in Settings to restore registration.")
        case .approvalRequired, .identityChanged, .accountDeviceRemoved:
            String(localized: "Link this iPhone again in Settings and approve it in Alondra on your Mac.")
        case .localNetworkDenied: String(localized: "Allow Local Network access for Alondra in iPhone Settings, then try again.")
        case .incompatible, .invalidRequest: String(localized: "Update Alondra on your Mac and iPhone, then try again.")
        case .rejected: String(localized: "Check this iPhone’s approval in Alondra on your Mac. If it is approved, run alondra report on your Mac.")
        case .local: String(localized: "Update Alondra on this iPhone. If it still fails, run alondra report on your Mac.")
        }
        return "\(subject): \(action)"
    }

    public static func retryDelay(attempt: Int) -> Duration {
        .seconds(min(5 * (1 << min(max(0, attempt), 6)), 300))
    }

    public static func response(status: Int, body: Data, operation: Operation) -> Self {
        struct Body: Decodable {
            let kind: String?
            let current_revision: UInt64?
            let reason: String?
        }
        let decoded = try? JSONDecoder().decode(Body.self, from: body)
        if status == 410, decoded?.kind == "outcome_unknown", decoded?.reason == "expired" {
            return Self(operation: operation, reason: .expired)
        }
        if let raw = decoded?.kind, let rejection = Rejection(rawValue: raw), rejection.status == status {
            return Self(operation: operation, reason: reason(rejection, revision: decoded?.current_revision))
        }
        return Self(operation: operation, reason: statusReason(status, operation: operation))
    }

    public static func classify(_ error: any Error, operation: Operation) -> Self {
        if let failure = error as? Self { return failure }
        if case PushPreferencesError.stale(let revision) = error {
            return Self(operation: operation, reason: .stalePreferences(revision))
        }
        if let api = error as? APIError {
            let status: Int
            let kind: String?
            switch api {
            case .unauthorized: status = 401; kind = nil
            case .forbidden(let value, _, _): status = 403; kind = value
            case .conflict(let value, _, _): status = 409; kind = value
            case .server(let value, let code, _, _): status = value; kind = code
            case .unexpected(let value): status = value; kind = nil
            case .unavailable: status = 503; kind = nil
            }
            let reason = kind.flatMap(Rejection.init(rawValue:)).flatMap { $0.status == status ? Self.reason($0, revision: nil) : nil }
                ?? statusReason(status, operation: operation)
            return Self(operation: operation, reason: reason)
        }
        if let relay = error as? RelayWireError {
            let reason: Reason = switch relay {
            case .unauthorized: .authenticationRequired
            case .admissionFull: .capacity
            case .server(let status): statusReason(status, operation: .relay)
            case .outcomeUnknown, .expired: .unavailable
            default: .incompatible
            }
            let origin: Operation = switch relay {
            case .unauthorized, .server, .admissionFull: .relay
            default: operation
            }
            return Self(operation: origin, reason: reason)
        }
        let reason: Reason = switch error {
        case is URLError, is DeadlineExceeded, IrohCarrierError.unavailable, IrohCarrierError.noAnswer,
             IrohLinkError.notSent, IrohLinkError.noAnswer,
             DirectRouteError.unreachable, SealedRequestError.outcomeUnknown, OperationError.outcomeUnknown, OperationError.sealExpired: .unavailable
        case IrohCarrierError.refused: .rejected
        case DirectSessionError.deviceRevoked: .approvalRequired
        case DirectRouteError.identityChanged: .identityChanged
        case DirectRouteError.localNetworkDenied: .localNetworkDenied
        case AccountDeviceError.removedFromWeb: .accountDeviceRemoved
        case SealedRequestError.invalidResponse: .incompatible
        default: .local
        }
        return Self(operation: operation, reason: reason)
    }

    private static func reason(_ rejection: Rejection, revision: UInt64?) -> Reason {
        switch rejection {
        case .notEnrolled: .notEnrolled
        case .stalePreferences:
            if let revision, revision < 9_007_199_254_740_991 { .stalePreferences(revision) } else { .incompatible }
        case .unsupportedRoute: .incompatible
        case .approvalRevoked: .approvalRequired
        case .capacity: .capacity
        case .expired: .expired
        default: .invalidRequest(rejection)
        }
    }

    private static func statusReason(_ status: Int, operation: Operation) -> Reason {
        switch status {
        case 401: .authenticationRequired
        case 403: operation == .relay ? .authenticationRequired : .approvalRequired
        case 404, 405, 426: .incompatible
        case 408, 500...599: .unavailable
        case 429: .capacity
        default: .rejected
        }
    }
}

public enum RegistrationState: Equatable, Sendable {
    case idle, sending, registered, failed([RegistrationFailure])

    public var failures: [RegistrationFailure] {
        if case .failed(let failures) = self { failures } else { [] }
    }
}
