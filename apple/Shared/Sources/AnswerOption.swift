import Foundation

/// Option labels that alondrad writes and later matches, so they travel to and from the
/// server unchanged; only what the person sees is translated.
public enum AnswerOption {
    public static let approve = "Approve"
    public static let requestChanges = "Request changes"
    public static let allow = "Allow"
    public static let deny = "Deny"
    public static let moreContext = "I need more context"

    public static func displayLabel(_ label: String) -> String {
        switch label {
        case approve: String(localized: "Approve", table: "AlondraShared", bundle: #bundle)
        case requestChanges: String(localized: "Request changes", table: "AlondraShared", bundle: #bundle)
        case allow: String(localized: "Allow", table: "AlondraShared", bundle: #bundle)
        case deny: String(localized: "Deny", table: "AlondraShared", bundle: #bundle)
        case moreContext: String(localized: "I need more context", table: "AlondraShared", bundle: #bundle)
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
