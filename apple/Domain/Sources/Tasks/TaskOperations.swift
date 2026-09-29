import AlondraContracts
import Foundation

/// A mutation may have reached the server even when its acknowledgement was lost: `unknown`
/// keeps the caller's draft and attempt ID for a retry.
public enum TaskMutationOutcome: Equatable, Sendable {
    case confirmed
    case unknown
    case rejected(String)
    case obsolete
}
