/// The «Viewed» box GitHub keeps for each file of a pull request, for the account the Mac is signed
/// in with. `changed` is a box GitHub cleared by itself because a push changed the file after it
/// was ticked.
public enum FileViewed: String, Codable, Equatable, Hashable, Sendable {
    case viewed, unviewed, changed
}

/// The boxes of a pull request's files as they were read with its diff, at that head.
public struct FileMarks: Equatable, Sendable {
    public let prID: String
    public let head: String
    public var files: [String: FileViewed]

    public init(prID: String, head: String, files: [String: FileViewed]) {
        self.prID = prID
        self.head = head
        self.files = files
    }
}
