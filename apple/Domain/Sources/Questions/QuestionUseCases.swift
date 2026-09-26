import AlondraContracts
import Foundation

/// Stored attachment identifiers, never local paths or bytes, cross this feature boundary.
/// H's adapter uploads selected files before inserting their identifiers in a draft.
public struct QuestionDraft: Equatable, Sendable {
    public var text = ""
    public var selectedOption: String?
    public var attachmentIDs: [String] = []

    public init() {}
}

public enum QuestionOperation: Equatable, Sendable {
    case answer(id: UInt64, text: String, option: String?, attachments: [String])
    case moreContext(id: UInt64, note: String, attachments: [String])
    case reviewReport(id: UInt64, verdict: ReviewVerdict)
}

public struct QuestionWorkIdentity: Hashable, Sendable {
    public let server: URL
    public let deviceID: String
    public let credentialRevision: String
    public let generation: UInt64

    public init(server: URL, deviceID: String, credentialRevision: String, generation: UInt64) {
        self.server = server
        self.deviceID = deviceID
        self.credentialRevision = credentialRevision
        self.generation = generation
    }
}

public struct ReportFeedbackRequest: Equatable, Sendable {
    public let reportID: UInt64
    public let sessionID: String
    public let text: String
    public let attachmentIDs: [String]

    public init(reportID: UInt64, sessionID: String, text: String, attachmentIDs: [String]) {
        self.reportID = reportID
        self.sessionID = sessionID
        self.text = text
        self.attachmentIDs = attachmentIDs
    }
}

public enum QuestionWorkOutcome: Equatable, Sendable {
    case confirmed
    case conflict(String)
    case forbidden(String)
    case failed(String)
    case uncertain
    case uncertainCommand(UUID)
    case obsolete
}

public struct QuestionWorkService: Sendable {
    public var submit: @Sendable (QuestionOperation) async -> QuestionWorkOutcome
    /// Must own one durable send-then-read transaction per identity/report. See INTEGRATION.md.
    public var submitReportFeedback: @Sendable (ReportFeedbackRequest, QuestionWorkIdentity) async -> QuestionWorkOutcome
    public var document: @Sendable (String) async throws -> String
    public var retryReportRead: @Sendable (UUID) async -> Void

    public init(
        submit: @escaping @Sendable (QuestionOperation) async -> QuestionWorkOutcome,
        submitReportFeedback: @escaping @Sendable (ReportFeedbackRequest, QuestionWorkIdentity) async -> QuestionWorkOutcome = { _, _ in .failed("Durable report feedback is unavailable") },
        document: @escaping @Sendable (String) async throws -> String,
        retryReportRead: @escaping @Sendable (UUID) async -> Void = { _ in }
    ) {
        self.submit = submit
        self.submitReportFeedback = submitReportFeedback
        self.document = document
        self.retryReportRead = retryReportRead
    }
}

public enum QuestionWorkError: Error, Equatable, Sendable {
    case unavailable
    case invalidOption
    case explanationRequired
    case answerRequired
    case noFeedback
    case notReviewable
    case menuRequiresOption
    case menuDoesNotAcceptAttachments
}

/// The daemon's option labels are protocol values. UI translations must not be sent back.
public enum QuestionExplanationPolicy: Equatable, Sendable { case phone, desktop }

public struct AnswerQuestionUseCase: Sendable {
    public init() {}

    public func answer(_ question: Question, draft: QuestionDraft, explanationPolicy: QuestionExplanationPolicy = .phone) throws -> QuestionOperation {
        var text = draft.text.trimmingCharacters(in: .whitespacesAndNewlines)
        // The original desktop option gesture used the selected label as its body
        // unless the option explicitly requested written explanation.
        if explanationPolicy == .desktop, text.isEmpty,
           let label = draft.selectedOption, let selected = question.options.first(where: { $0.label == label }),
           !Self.needsExplanation(selected, policy: .desktop) { text = label }
        if question.isTerminalMenu {
            guard let option = draft.selectedOption,
                  question.options.contains(where: { $0.label == option }) else {
                throw QuestionWorkError.menuRequiresOption
            }
            guard draft.attachmentIDs.isEmpty else { throw QuestionWorkError.menuDoesNotAcceptAttachments }
            return .answer(id: question.id, text: option, option: option, attachments: [])
        }
        if let option = draft.selectedOption {
            guard question.options.contains(where: { $0.label == option }) ||
                    (question.isPlan && [AnswerOption.approve, AnswerOption.requestChanges].contains(option))
            else { throw QuestionWorkError.invalidOption }
            if let selected = question.options.first(where: { $0.label == option }),
               Self.needsExplanation(selected, policy: explanationPolicy), text.isEmpty,
               explanationPolicy == .desktop || draft.attachmentIDs.isEmpty {
                throw QuestionWorkError.explanationRequired
            }
            if (question.isPlan && option == AnswerOption.requestChanges ||
                question.isAction && option == AnswerOption.cannot),
               text.isEmpty, draft.attachmentIDs.isEmpty {
                throw QuestionWorkError.explanationRequired
            }
        }
        guard !text.isEmpty || draft.selectedOption != nil || !draft.attachmentIDs.isEmpty else {
            throw QuestionWorkError.answerRequired
        }
        return .answer(id: question.id, text: text.isEmpty && draft.attachmentIDs.isEmpty ? draft.selectedOption ?? "" : text,
                       option: draft.selectedOption, attachments: draft.attachmentIDs)
    }

    public func moreContext(_ question: Question, draft: QuestionDraft) -> QuestionOperation {
        .moreContext(id: question.id, note: draft.text.trimmingCharacters(in: .whitespacesAndNewlines),
                     attachments: draft.attachmentIDs)
    }

    public func checkedMoreContext(_ question: Question, draft: QuestionDraft) throws -> QuestionOperation {
        guard !question.isTerminalMenu else { throw QuestionWorkError.menuRequiresOption }
        return moreContext(question, draft: draft)
    }

    public static func needsExplanation(_ option: QuestionOption, policy: QuestionExplanationPolicy = .phone) -> Bool {
        let description = option.description?.lowercased() ?? ""
        return description.hasPrefix("write") || description.hasPrefix("escribe")
            || (policy == .phone && [AnswerOption.deny, AnswerOption.requestChanges].contains(option.label))
    }
}

public struct ReviewDocumentUseCase: Sendable {
    public init() {}

    public func feedback(
        for report: Report, comments: [ReviewComment], general: String, author: String,
        attachmentIDs: [String]
    ) throws -> ReportFeedbackRequest {
        let text = ReviewFeedback.format(comments, general: general, author: author)
        guard !text.isEmpty || !attachmentIDs.isEmpty else { throw QuestionWorkError.noFeedback }
        return ReportFeedbackRequest(reportID: report.id, sessionID: report.sessionId,
                                     text: text, attachmentIDs: attachmentIDs)
    }

    public func review(_ report: Report, verdict: ReviewVerdict) throws -> QuestionOperation {
        guard !report.artifacts.isEmpty else { throw QuestionWorkError.notReviewable }
        return .reviewReport(id: report.id, verdict: verdict)
    }
}
