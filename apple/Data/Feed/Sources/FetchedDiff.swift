import AlondraContract

/// A pull request's unified diff as the Mac sent it; `truncated` when it was cut to fit. `marks`
/// are GitHub's «Viewed» boxes at the same head, nil when the Mac did not send them.
public struct FetchedDiff: Equatable, Sendable {
    public let text: String
    public let truncated: Bool
    public let marks: FileMarks?

    public init(text: String, truncated: Bool = false, marks: FileMarks? = nil) {
        self.text = text
        self.truncated = truncated
        self.marks = marks
    }
}

/// Why the Mac did not save a «Viewed» box: the screen showed another head or a closed pull
/// request (`stale`), or it can never take this box (`refused`). Anything else is worth retrying.
public enum FileMarkError: Error, Equatable, Sendable {
    case stale, refused
}
