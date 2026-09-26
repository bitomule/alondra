import Foundation

/// Option labels that alondrad writes and later matches, so they travel to and from the
/// server unchanged; only what the person sees is translated.
public enum AnswerOption {
    public static let approve = "Approve"
    public static let requestChanges = "Request changes"
    public static let allow = "Allow"
    public static let deny = "Deny"
    public static let moreContext = "I need more context"
    public static let done = "Done"
    public static let cannot = "I can't / explain"
    public static let keepIdleNode = "Keep it"
    public static let discardIdleNode = "Close it and discard its work"
    public static let resumeIdleNode = "Resume it"

}
