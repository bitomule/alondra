import AlondraTransport
import Foundation

/// Why an answer can never reach the Mac, however many times it is sent.
public enum ActionDeliveryError: Error, Equatable, Sendable {
    /// The Mac's data was reset after the answer was given: what it answered no longer exists.
    case dataReset
    /// The Mac already settled this answer and no longer holds what it replied.
    case answeredBefore
}

/// Whether sending an answer again can still change how it ends.
enum ActionDeliveryDisposition: Equatable {
    case retry
    case final

    init(_ error: any Error) {
        switch error {
        case let error as APIError:
            switch error {
            case .unauthorized, .forbidden: self = .final
            case let .conflict(_, detail, _): self = detail == "outcome_unknown" ? .retry : .final
            case let .server(status, _, _, _): self = Self.retries(status) ? .retry : .final
            case let .unexpected(status): self = Self.retries(status) ? .retry : .final
            case .unavailable: self = .retry
            }
        case let error as OperationError:
            self = error == .outcomeUnknown || error == .sealExpired ? .retry : .final
        case let error as ViewActionIntentError:
            self = error == .capacityExceeded ? .retry : .final
        case is ActionDeliveryError, is FeedError, is DirectSessionError:
            self = .final
        case let error as RelaySessionError:
            self = error == .operationIdentityConflict || error == .invalidRequest ? .final : .retry
        default:
            self = .retry
        }
    }

    private static func retries(_ status: Int) -> Bool {
        status >= 500 || status == 408 || status == 429
    }
}
