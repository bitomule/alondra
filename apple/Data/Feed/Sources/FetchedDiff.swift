/// A pull request's unified diff as the Mac sent it; `truncated` when it was cut to fit.
public struct FetchedDiff: Equatable, Sendable {
    public let text: String
    public let truncated: Bool

    public init(text: String, truncated: Bool = false) {
        self.text = text
        self.truncated = truncated
    }
}
