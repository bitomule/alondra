public struct DiffFile: Identifiable, Hashable, Sendable {
    public let path: String
    public let lines: [DiffLine]
    public var id: String { path }
    public var additions: Int { lines.filter { $0.kind == .added }.count }
    public var deletions: Int { lines.filter { $0.kind == .removed }.count }
}
