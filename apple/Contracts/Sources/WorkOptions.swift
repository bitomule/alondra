import Foundation

/// A value sent to alondrad as a JSON boolean or string.
public enum OptionChoice: Codable, Hashable, Sendable {
    case toggle(Bool)
    case value(String)

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let enabled = try? container.decode(Bool.self) {
            self = .toggle(enabled)
        } else {
            self = .value(try container.decode(String.self))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .toggle(let enabled): try container.encode(enabled)
        case .value(let value): try container.encode(value)
        }
    }
}

/// One server-defined option. Display wording is supplied by Presentation.
public struct OptionSpec: Decodable, Identifiable, Hashable, Sendable {
    public enum Kind: String, Decodable, Sendable {
        case choice, toggle
    }

    public struct Value: Decodable, Identifiable, Hashable, Sendable {
        public let id: String
        public let label: String
        public let description: String?
    }

    public let id: String
    public let label: String
    public let kind: Kind
    public let values: [Value]
    public let `default`: OptionChoice
    public let builtin: Bool

    private enum CodingKeys: String, CodingKey {
        case id, label, kind, values, `default`, builtin
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        label = try container.decode(String.self, forKey: .label)
        kind = try container.decode(Kind.self, forKey: .kind)
        values = try container.decodeIfPresent([Value].self, forKey: .values) ?? []
        self.default = try container.decode(OptionChoice.self, forKey: .default)
        builtin = try container.decodeIfPresent(Bool.self, forKey: .builtin) ?? false
    }
}

public struct OptionsCatalog: Decodable, Sendable {
    public let project: String?
    public let options: [OptionSpec]
    public let error: String?
}

public struct ProjectList: Decodable, Sendable {
    public struct Project: Decodable, Hashable, Sendable {
        public let name: String
    }

    public let projects: [Project]
}

/// What the New screen submits: the request text with its project, options, issue and
/// uploaded attachments, sent as one `create_request`.
public struct NewRequestBody: Sendable, Equatable {
    public let text: String
    public var attachments: [DomainV2.AttachmentRef] = []
    public let project: String?
    public let options: [String: OptionChoice]
    public var issue: String?

    public init(text: String, attachments: [DomainV2.AttachmentRef] = [], project: String?, options: [String: OptionChoice], issue: String?) {
        self.text = text
        self.attachments = attachments
        self.project = project
        self.options = options
        self.issue = issue
    }
}
