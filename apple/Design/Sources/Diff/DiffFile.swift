public struct DiffFile: Identifiable, Hashable, Sendable {
    public let path: String
    public let lines: [DiffLine]
    public var id: String { path }
    public var additions: Int { lines.filter { $0.kind == .added }.count }
    public var deletions: Int { lines.filter { $0.kind == .removed }.count }

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

    public var startsCollapsed: Bool {
        isGenerated || additions + deletions > Self.foldedChangeThreshold
    }

    public func commentCount(in comments: [String: String]) -> Int {
        Set(lines.compactMap(\.anchor)).filter { comments[$0] != nil }.count
    }
}
