import Foundation
import AlondraContracts

/// Describes the effect for reconciliation. The payload stays with the caller until it has
/// been sent once; no closure or payload is retained for an automatic replay.
public enum CommandKind: Hashable, Sendable, Codable {
    case domain(DomainCommandKind)
    case resolveDomainDelivery(DomainDeliveryDecision)
    case task(String)
    /// Journals written before the domain v2 cutover. They are shown and dismissed, never
    /// reinterpreted as v2 operations.
    case answer(UInt64)
    case moreContext(UInt64)
    case markRead(UInt64)
    case review(UInt64)
    case send(String)
    case replyToReport(UInt64)
    case newRequest
    case closeNode(String)
    case retryOrchestrator(String)
    case cancelOrchestrator(String)
    case acknowledgeOrchestrator(String)
    case cancelTask(String)
    case reopenTask(String)
    case withdrawRequest(String)
    case archiveCompleted
    case mergePullRequest(UInt64)
    case resolveConflicts(UInt64)
    case requestChanges(UInt64)
    case askOnPullRequest(UInt64)
    case dismissPullRequest(UInt64)
    case resolveDelivery(DeliveryDecision)
}

/// One semantic v2 command: its wire kind and the subject it blocks until resolved.
public struct DomainCommandKind: Hashable, Sendable, Codable {
    public let action: String
    public let subject: String

    public init(action: String, subject: String) {
        self.action = action
        self.subject = subject
    }

    public init(_ intent: DomainIntent) {
        switch intent {
        case .createRequest: action = "create_request"
        case .closeRequest: action = "close_request"
        default: action = intent.actionKind?.rawValue ?? ""
        }
        subject = intent.subject
    }
}

/// Captured before dispatch; the resolution receipt must match this exact attempt version.
public struct DomainDeliveryDecision: Hashable, Sendable, Codable {
    public let attemptID: Int64
    public let version: UInt64
    public let retry: Bool

    public init(attemptID: Int64, version: UInt64, retry: Bool) {
        self.attemptID = attemptID
        self.version = version
        self.retry = retry
    }

    public func validate(_ resolution: DomainV2.DeliveryResolution) -> Bool {
        resolution.attemptId == attemptID && resolution.version == version + 1
            && resolution.state == (retry ? "retryable" : "discarded")
    }
}
