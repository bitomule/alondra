import Foundation

public enum BackgroundKind: String, Codable, Sendable, CaseIterable {
    case agent
    case shell
    case other

    public var label: String {
        switch self {
        case .agent: String(localized: "subagent", table: "AlondraShared", bundle: #bundle)
        case .shell: String(localized: "shell", table: "AlondraShared", bundle: #bundle)
        case .other: String(localized: "task", table: "AlondraShared", bundle: #bundle)
        }
    }

    func count(_ n: Int) -> String {
        switch self {
        case .agent: String(localized: "\(n) subagents", table: "AlondraShared", bundle: #bundle)
        case .shell: String(localized: "\(n) shells", table: "AlondraShared", bundle: #bundle)
        case .other: String(localized: "\(n) tasks", table: "AlondraShared", bundle: #bundle)
        }
    }

    public init(from decoder: Decoder) throws {
        self = BackgroundKind(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .other
    }
}

/// A subagent or background shell a node left running.
public struct BackgroundItem: Codable, Hashable, Sendable {
    public let kind: BackgroundKind
    public let label: String
    public let startedAt: Int64
    public let suspect: Bool?

    public init(kind: BackgroundKind, label: String, startedAt: Int64, suspect: Bool? = nil) {
        self.kind = kind
        self.label = label
        self.startedAt = startedAt
        self.suspect = suspect
    }

    /// Running for long while the node shows no other work: it may be hung.
    public var isSuspect: Bool { suspect == true }

    public static var sectionTitle: String {
        String(localized: "Running in the background", table: "AlondraShared", bundle: #bundle)
    }

    public static var possiblyHung: String {
        String(localized: "hung?", table: "AlondraShared", bundle: #bundle)
    }
}

/// "2 subagents · 1 shell": what a node runs in the background, counted by kind.
public struct BackgroundBadge: Sendable {
    public let counts: [(kind: BackgroundKind, count: Int)]
    public let suspect: Bool

    public init?(_ items: [BackgroundItem]) {
        let counts = BackgroundKind.allCases.compactMap { kind in
            let n = items.count(where: { $0.kind == kind })
            return n > 0 ? (kind: kind, count: n) : nil
        }
        guard !counts.isEmpty else { return nil }
        self.counts = counts
        suspect = items.contains(where: \.isSuspect)
    }

    public var text: String {
        let parts = counts.map { $0.kind.count($0.count) }
        return (suspect ? parts + [BackgroundItem.possiblyHung] : parts).joined(separator: " · ")
    }
}
