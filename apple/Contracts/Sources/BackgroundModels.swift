import Foundation

public enum BackgroundKind: String, Codable, Sendable, CaseIterable {
    case agent
    case shell
    case other

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

}
