import AlondraContracts
import Foundation

extension AnswerOption {
    public static func displayLabel(_ label: String) -> String {
        switch label {
        case approve: String(localized: "Approve", table: "AlondraShared", bundle: #bundle)
        case requestChanges: String(localized: "Request changes", table: "AlondraShared", bundle: #bundle)
        case allow: String(localized: "Allow", table: "AlondraShared", bundle: #bundle)
        case deny: String(localized: "Deny", table: "AlondraShared", bundle: #bundle)
        case moreContext: String(localized: "I need more context", table: "AlondraShared", bundle: #bundle)
        case done: String(localized: "Done", table: "AlondraShared", bundle: #bundle)
        case cannot: String(localized: "I can't / explain", table: "AlondraShared", bundle: #bundle)
        case keepIdleNode: String(localized: "Keep it", table: "AlondraShared", bundle: #bundle)
        case discardIdleNode: String(localized: "Close it and discard its work", table: "AlondraShared", bundle: #bundle)
        case resumeIdleNode: String(localized: "Resume it", table: "AlondraShared", bundle: #bundle)
        default: label
        }
    }

    public static func displayDescription(_ description: String) -> String {
        switch description {
        case "Write what to change": String(localized: "Write what to change", table: "AlondraShared", bundle: #bundle)
        case "Write the reason": String(localized: "Write the reason", table: "AlondraShared", bundle: #bundle)
        default: description
        }
    }
}

extension QuestionOption {
    public var displayLabel: String { AnswerOption.displayLabel(label) }
    public var displayDescription: String? { description.map(AnswerOption.displayDescription) }
}
