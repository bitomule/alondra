import Foundation

public struct ViewFeed: Codable, Equatable, Sendable {
    public let protocolVersion: UInt32
    public let cursor: String
    public let dataEpoch: String
    public let status: ViewStatus
    public let stale: [String]
    public let reset: Bool
    public let superseded: Bool
    enum CodingKeys: String, CodingKey { case protocolVersion = "protocol"; case cursor, dataEpoch = "data_epoch", status, stale, reset, superseded }
}

public struct OpenViewVersion: Codable, Equatable, Sendable {
    public let key: String
    public let version: String
    public init(key: String, version: String) { self.key = key; self.version = version }
    public var feedValue: String { "\(key)@\(version)" }
}

public struct ActionInput: Codable, Equatable, Sendable {
    public let text: String?
    public let attachments: [String]?
    public let comments: [ActionComment]?
    public let fields: [String: JSONValue]?
    enum CodingKeys: String, CodingKey { case text, attachments, comments, fields }
    public init(text: String? = nil, attachments: [String]? = nil, comments: [ActionComment]? = nil, fields: [String: JSONValue]? = nil) {
        self.text = text; self.attachments = attachments; self.comments = comments; self.fields = fields
    }
}

public struct ActionComment: Codable, Equatable, Sendable {
    public let anchor: String
    public let text: String
    public let quote: String?
    public init(anchor: String, text: String, quote: String? = nil) { self.anchor = anchor; self.text = text; self.quote = quote }
}

public struct ActionRequest: Codable, Equatable, Sendable {
    public let protocolVersion: UInt32
    public let commandId: String
    public let dataEpoch: String
    public let client: ViewClient
    public let lang: String
    public let actionId: String
    public let optionId: String?
    public let input: ActionInput
    enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol", commandId = "command_id", dataEpoch = "data_epoch", client, lang
        case actionId = "action_id", optionId = "option_id", input
    }
    public init(protocolVersion: UInt32 = ContractCodec.protocolVersion, commandId: String, dataEpoch: String, client: ViewClient, lang: String, actionId: String, optionId: String? = nil, input: ActionInput = ActionInput()) {
        self.protocolVersion = protocolVersion; self.commandId = commandId; self.dataEpoch = dataEpoch; self.client = client
        self.lang = lang; self.actionId = actionId; self.optionId = optionId; self.input = input
    }
}

public enum ActionOutcomeKind: String, Codable, Equatable, Sendable { case applied, noLongerAvailable = "no_longer_available", changed, rejected }
public struct ActionResponse: Codable, Equatable, Sendable {
    public let protocolVersion: UInt32
    public let outcome: ActionOutcomeKind
    public let stamp: String?
    public let revision: UInt64?
    public let message: String?
    public let field: String?
    public let confirm: ActionConfirmation?
    enum CodingKeys: String, CodingKey { case protocolVersion = "protocol", outcome, stamp, revision, message, field, confirm }
}
public struct ActionConfirmation: Codable, Equatable, Sendable {
    public let actionId: String
    public let button: String
    enum CodingKeys: String, CodingKey { case actionId = "action_id", button }
}
