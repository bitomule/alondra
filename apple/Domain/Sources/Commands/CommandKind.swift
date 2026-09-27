import Foundation
import AlondraContracts

/// Describes the effect for reconciliation. The payload stays with the caller until it has
/// been sent once; no closure or payload is retained for an automatic replay.
public enum CommandKind: Hashable, Sendable, Codable {
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
    case task(String)
    case resolveDelivery(DeliveryDecision)


}
