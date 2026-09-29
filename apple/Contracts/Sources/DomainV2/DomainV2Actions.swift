import Foundation

extension DomainV2 {
    /// Action kinds the server offers in `available_actions`. A kind this build does not
    /// know is not rendered: the server decides availability, the client never invents one.
    public enum ActionKind: String, Codable, CaseIterable, Sendable {
        case markRead = "mark_read"
        case answerQuestion = "answer_question"
        case acceptDelivery = "accept_delivery"
        case submitFeedback = "submit_feedback"
        case authorizeMerge = "authorize_merge"
        case cancelTask = "cancel_task"
        case reopenTask = "reopen_task"
        case archiveTask = "archive_task"
        case unarchiveTask = "unarchive_task"
    }

    /// Typed error codes from the command service and domain reads. Unknown codes stay
    /// available through `DomainError.code`.
    public enum ErrorCode: String, CaseIterable, Sendable {
        case notFound = "not_found"
        case notAssigned = "not_assigned"
        case forbidden
        case invalidInput = "invalid_input"
        case invalidActionForKind = "invalid_action_for_kind"
        case conflict
        case staleRevision = "stale_revision"
        case staleHead = "stale_head"
        case staleEpoch = "stale_epoch"
        case staleGeneration = "stale_generation"
        case taskNotActive = "task_not_active"
        case taskAlreadyHasPr = "task_already_has_pr"
        case prOwnedByOtherTask = "pr_owned_by_other_task"
        case subjectAlreadyLinked = "subject_already_linked"
        case acceptanceMissing = "acceptance_missing"
        case acceptanceStale = "acceptance_stale"
        case mergeNotConfirmed = "merge_not_confirmed"
        case stackNotReady = "stack_not_ready"
        case dependencyNotDone = "dependency_not_done"
        case dependencyCycle = "dependency_cycle"
        case alreadyRead = "already_read"
        case withdrawn
        case incompatibleProtocol = "incompatible_protocol"
        case idempotencyConflict = "idempotency_conflict"
        case deliveryUncertain = "delivery_uncertain"
        case payloadTooLarge = "payload_too_large"
        case contentUnavailable = "content_unavailable"
        case externalReadFailed = "external_read_failed"
        case storageUnavailable = "storage_unavailable"
    }
}

extension DomainV2.AvailableAction {
    public var actionKind: DomainV2.ActionKind? { DomainV2.ActionKind(rawValue: kind) }
    public var disabledCode: DomainV2.ErrorCode? { disabledReason.flatMap(DomainV2.ErrorCode.init(rawValue:)) }
}

extension DomainV2.DomainError {
    public var errorCode: DomainV2.ErrorCode? { DomainV2.ErrorCode(rawValue: code) }
}
