public struct DiffFile: Identifiable, Hashable, Sendable {
    public enum Status: Sendable { case added, deleted, renamed, modified }

    public let path: String
    public let oldPath: String?
    public let status: Status
    public let isBinary: Bool
    public let lines: [DiffLine]
    public let additions: Int
    public let deletions: Int
    public var id: String { path }

    public init(path: String, oldPath: String? = nil, status: Status = .modified, isBinary: Bool = false, lines: [DiffLine]) {
        self.path = path
        self.oldPath = oldPath
        self.status = status
        self.isBinary = isBinary
        self.lines = lines
        additions = lines.filter { $0.kind == .added }.count
        deletions = lines.filter { $0.kind == .removed }.count
    }

    public static let foldedChangeThreshold = 400
    static let generatedNames: Set<String> = [
        "Cargo.lock", "Package.resolved", "MODULE.bazel.lock", "package-lock.json", "yarn.lock", "pnpm-lock.yaml",
        "Podfile.lock", "Gemfile.lock", "go.sum", "poetry.lock", "uv.lock", "flake.lock",
    ]

    public var isGenerated: Bool {
        let components = path.split(separator: "/")
        let name = components.last.map(String.init) ?? path
        return Self.generatedNames.contains(name) || name.hasSuffix(".xcstrings") || components.contains("__Snapshots__")
    }

    public var isLarge: Bool { additions + deletions > Self.foldedChangeThreshold }

    public var hasCode: Bool { lines.contains { $0.kind != .note } }

    public var startsCollapsed: Bool {
        isBinary || !hasCode || isGenerated || isLarge
    }

    public func commentCount(in comments: [String: String]) -> Int {
        Set(lines.compactMap(\.anchor)).filter { comments[$0] != nil }.count
    }
}
