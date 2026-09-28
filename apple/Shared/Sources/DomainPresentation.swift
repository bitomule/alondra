import AlondraContracts
import AlondraDomain
import Foundation

extension DomainIntentError {
    public var formMessage: String {
        switch self {
        case .notFound: String(localized: "This item is no longer available.", table: "AlondraShared", bundle: #bundle)
        case .unavailable(let reason):
            reason.flatMap(DomainV2.ErrorCode.init(rawValue:))?.localizedMessage
                ?? String(localized: "This action is not available any more. Refresh and try again.", table: "AlondraShared", bundle: #bundle)
        case .optionRequired: String(localized: "Choose an option.", table: "AlondraShared", bundle: #bundle)
        case .inputRequired: String(localized: "Fill in the required field.", table: "AlondraShared", bundle: #bundle)
        case .inputInvalid: String(localized: "This option doesn’t take that answer.", table: "AlondraShared", bundle: #bundle)
        case .feedbackRequired: String(localized: "Write your feedback first.", table: "AlondraShared", bundle: #bundle)
        case .requestRequired: String(localized: "Describe the request first.", table: "AlondraShared", bundle: #bundle)
        case .methodNotAllowed: String(localized: "This merge method is not allowed for this pull request.", table: "AlondraShared", bundle: #bundle)
        }
    }
}

extension DomainV2.ErrorCode {
    /// Localized copy for the typed codes a person can act on. Anything else shows the
    /// server's own message.
    public var localizedMessage: String? {
        switch self {
        case .staleRevision, .staleHead, .acceptanceStale:
            String(localized: "This changed since you opened it. Review the latest version before deciding.", table: "AlondraShared", bundle: #bundle)
        case .staleEpoch:
            String(localized: "This Mac’s data was reset. Reopen the item before deciding.", table: "AlondraShared", bundle: #bundle)
        case .taskNotActive:
            String(localized: "This task is no longer active.", table: "AlondraShared", bundle: #bundle)
        case .invalidActionForKind:
            String(localized: "This action doesn’t apply to this item.", table: "AlondraShared", bundle: #bundle)
        case .forbidden:
            String(localized: "You can’t do this from this device.", table: "AlondraShared", bundle: #bundle)
        case .notFound:
            String(localized: "This item is no longer available.", table: "AlondraShared", bundle: #bundle)
        case .acceptanceMissing:
            String(localized: "Accept the result before this can continue.", table: "AlondraShared", bundle: #bundle)
        case .incompatibleProtocol:
            String(localized: "This Mac runs a different version of Alondra. Update the app or the Mac.", table: "AlondraShared", bundle: #bundle)
        case .idempotencyConflict:
            String(localized: "This action was already sent with different content.", table: "AlondraShared", bundle: #bundle)
        default: nil
        }
    }
}

extension DomainCommandRejection {
    public var localizedMessage: String { errorCode?.localizedMessage ?? message }
}
