import AlondraDesign

/// A pull request's diff as the Mac window holds it: the head it was read at, its files, and whether it was cut.
public struct MacDiff: Equatable, Sendable {
    public let digest: String
    public let files: [DiffFile]
    public let truncated: Bool

    public init(digest: String, files: [DiffFile], truncated: Bool) {
        self.digest = digest
        self.files = files
        self.truncated = truncated
    }
}
