import Foundation

public enum AccountProvider: String, Codable, Equatable, Hashable, Sendable, KnownValueDecodable {
    case claude, codex
    public var displayName: String { self == .claude ? "Claude" : "Codex" }
}

public struct ViewProtocolRange: Codable, Equatable, Sendable {
    public let min: UInt32
    public let max: UInt32
}

public struct ViewService: Codable, Equatable, Sendable {
    public let version: String
    public let viewsProtocol: ViewProtocolRange
    public let capabilities: [String]
    enum CodingKeys: String, CodingKey { case version, viewsProtocol = "views_protocol", capabilities }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = try c.decode(String.self, forKey: .version)
        viewsProtocol = try c.decode(ViewProtocolRange.self, forKey: .viewsProtocol)
        capabilities = try c.decodeIfPresent([String].self, forKey: .capabilities) ?? []
    }
}

public struct AccountIdentity: Codable, Equatable, Hashable, Sendable {
    public let provider: AccountProvider
    public let account: String
    public var wireID: String { "\(provider.rawValue)/\(account)" }
}

public struct AccountPresentation: Codable, Equatable, Sendable {
    public let usageSummary: String?
    public let quotaSource: String?
    public let modelCatalog: AccountModelCatalogPresentation?
    public let key: AccountIdentity
    public let position: UInt32
    public let availability: Label
    public let reason: String?
    public let email: String?
    public let plan: String?
    public let activeNodes: UInt32
    public let freshness: Label
    public let windows: [AccountWindowPresentation]
    enum CodingKeys: String, CodingKey { case key, position, availability, reason, email, plan, freshness, windows; case activeNodes = "active_nodes", modelCatalog = "model_catalog", usageSummary = "usage_summary", quotaSource = "quota_source" }
}

public struct AccountWindowPresentation: Codable, Equatable, Identifiable, Sendable {
    public let kind: String
    public let label: String
    public let usedPercent: UInt32
    public let measuredAt: Int64
    public let resetsAt: Int64?
    public let stale: Bool
    public let exhausted: Bool
    public var id: String { kind }
    enum CodingKeys: String, CodingKey { case kind, label, stale, exhausted; case usedPercent = "used_percent", measuredAt = "measured_at", resetsAt = "resets_at" }
}

public struct TaskModelPresentation: Codable, Equatable, Sendable {
    public let requested: String
    public let effective: String?
    public let pending: String?
    public let reason: String?
}

public struct AccountConfigurationPresentation: Codable, Equatable, Sendable {
    public let version: String
}

public struct AccountModelCatalogPresentation: Codable, Equatable, Sendable {
    public let account: AccountIdentity
    public let revision: String
    public let models: [CatalogModelPresentation]
    public let nativeDefault: String?
    public let measuredAt: Int64
    enum CodingKeys: String, CodingKey { case account, revision, models; case nativeDefault = "native_default", measuredAt = "measured_at" }
}

public struct CatalogModelPresentation: Codable, Equatable, Identifiable, Sendable {
    public let model: String
    public let displayName: String
    public let reasoningEfforts: [String]
    public let defaultReasoningEffort: String?
    public let contextWindow: UInt64?
    public let inputModalities: [String]
    public let isDefault: Bool
    public let hidden: Bool
    public var id: String { model }
    enum CodingKeys: String, CodingKey {
        case model, hidden
        case displayName = "display_name", reasoningEfforts = "reasoning_efforts", defaultReasoningEffort = "default_reasoning_effort"
        case contextWindow = "context_window", inputModalities = "input_modalities", isDefault = "is_default"
    }
}
