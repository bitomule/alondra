import Foundation

/// Wire models for the server-rendered views protocol. Keep these DTOs independent
/// from the existing domain and presentation models.
public enum ContractCodec {
    public static let protocolVersion: UInt32 = 1

    public static func decode<Value: Decodable>(_ type: Value.Type, from data: Data) throws -> Value {
        try JSONDecoder().decode(type, from: data)
    }

    public static func encode<Value: Encodable>(_ value: Value) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return try encoder.encode(value)
    }
}

public enum ViewClient: String, Codable, Equatable, Sendable { case ios, mac, tui }

public enum ViewFetch: Decodable, Equatable, Sendable {
    case document(ViewDocument)
    case unchanged(cursor: String)

    private enum CodingKeys: String, CodingKey { case `protocol`, unchanged, cursor }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        if try container.decodeIfPresent(Bool.self, forKey: .unchanged) == true {
            let version = try container.decode(UInt32.self, forKey: .protocol)
            guard version == ContractCodec.protocolVersion else {
                throw DecodingError.dataCorruptedError(forKey: .protocol, in: container, debugDescription: "Unsupported views protocol")
            }
            self = .unchanged(cursor: try container.decode(String.self, forKey: .cursor))
        } else {
            self = .document(try ViewDocument(from: decoder))
        }
    }
}

public enum Tone: String, Codable, Equatable, Sendable {
    case neutral, info, accent, success, warning, critical
    case yourTurn = "your_turn", unclear
}
public enum Level: String, Codable, Equatable, Sendable { case calm, warm, hot }
public enum Icon: String, Codable, Equatable, Sendable {
    case question, action, document, pullRequest = "pull_request", notice, delivery, task, request
    case node, orchestrator, mission, project, schedule, attachment, check, warning, merge, branch, history, machine
}
public enum Detail: String, Codable, Equatable, Sendable { case included, reference }
public enum TimeFormat: String, Codable, Equatable, Sendable { case relative, absolute, time }
public enum ContentKind: String, Codable, Equatable, Sendable { case document, diff, file }
public enum AttachmentKind: String, Codable, Equatable, Sendable { case image, text, binary }
public enum CheckState: String, Codable, Equatable, Sendable { case pass, fail, pending }
public enum Style: String, Codable, Equatable, Sendable { case primary, secondary, destructive, plain }
public enum Presentation: String, Codable, Equatable, Sendable { case button, options, menu }
public enum CommentKind: String, Codable, Equatable, Sendable { case blocks, diffLines = "diff_lines" }

public struct ViewDocument: Codable, Equatable, Sendable {
    public let protocolVersion: UInt32
    public let view: String
    public let key: String
    public let client: ViewClient
    public let version: String
    public let cursor: String
    public let dataEpoch: String
    public let title: String?
    public let status: ViewStatus
    public let sections: [ViewSection]
    public let empty: Notice?
    public let notices: [Notice]
    enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol", view, key, client, version, cursor, dataEpoch = "data_epoch", title, status, sections, empty, notices
    }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        protocolVersion = try c.decode(UInt32.self, forKey: .protocolVersion)
        view = try c.decode(String.self, forKey: .view); key = try c.decode(String.self, forKey: .key)
        client = try c.decode(ViewClient.self, forKey: .client); version = try c.decode(String.self, forKey: .version)
        cursor = try c.decode(String.self, forKey: .cursor); dataEpoch = try c.decode(String.self, forKey: .dataEpoch); title = try c.decodeIfPresent(String.self, forKey: .title)
        status = try c.decode(ViewStatus.self, forKey: .status); sections = try c.decode([ViewSection].self, forKey: .sections)
        empty = try c.decodeIfPresent(Notice.self, forKey: .empty); notices = try c.decodeIfPresent([Notice].self, forKey: .notices) ?? []
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(protocolVersion, forKey: .protocolVersion); try c.encode(view, forKey: .view); try c.encode(key, forKey: .key)
        try c.encode(client, forKey: .client); try c.encode(version, forKey: .version); try c.encode(cursor, forKey: .cursor); try c.encode(dataEpoch, forKey: .dataEpoch)
        try c.encodeIfPresent(title, forKey: .title); try c.encode(status, forKey: .status); try c.encode(sections, forKey: .sections)
        try c.encodeIfPresent(empty, forKey: .empty); if !notices.isEmpty { try c.encode(notices, forKey: .notices) }
    }
}

public struct ViewStatus: Codable, Equatable, Sendable {
    public let version: String
    public let summary: String
    public let badge: UInt32
    public let counts: [ViewCount]
    public let gauges: [Gauge]
    public let orchestrator: Label
    public let update: Notice?
}
public struct ViewCount: Codable, Equatable, Sendable { public let id: String; public let label: String; public let value: UInt32 }
public struct Gauge: Codable, Equatable, Sendable { public let id: String; public let label: String; public let value: Double; public let text: String; public let detail: String?; public let level: Level }
public struct Label: Codable, Equatable, Sendable { public let text: String; public let tone: Tone }
public struct ViewSection: Codable, Equatable, Sendable {
    public let id: String; public let title: String?; public let count: UInt32?; public let collapsed: Bool
    public let items: [ViewItem]; public let cardIds: [String]?; public let more: More?
    enum CodingKeys: String, CodingKey { case id, title, count, collapsed, items; case cardIds = "card_ids"; case more }
}
public struct More: Codable, Equatable, Sendable { public let key: String; public let remaining: UInt32 }

/// `Item` is internally tagged by the wire's `type` property.
public enum ViewItem: Codable, Equatable, Sendable {
    case card(Card), row(Row), markdown(Markdown), contentRef(ContentRef), facts(Facts), checks(Checks), timeline(Timeline), form(Form), notice(Notice), actions(ActionBar)
    private enum TagKeys: String, CodingKey { case type }
    public init(from decoder: Decoder) throws {
        let tag = try decoder.container(keyedBy: TagKeys.self)
        let type = try tag.decode(String.self, forKey: .type)
        switch type {
        case "card": self = .card(try Card(from: decoder))
        case "row": self = .row(try Row(from: decoder))
        case "markdown": self = .markdown(try Markdown(from: decoder))
        case "content_ref": self = .contentRef(try ContentRef(from: decoder))
        case "facts": self = .facts(try Facts(from: decoder))
        case "checks": self = .checks(try Checks(from: decoder))
        case "timeline": self = .timeline(try Timeline(from: decoder))
        case "form": self = .form(try Form(from: decoder))
        case "notice": self = .notice(try Notice(from: decoder))
        case "actions": self = .actions(try ActionBar(from: decoder))
        default: throw DecodingError.dataCorruptedError(forKey: .type, in: tag, debugDescription: "Unknown view item type: \(type)")
        }
    }
    public func encode(to encoder: Encoder) throws {
        var tag = encoder.container(keyedBy: TagKeys.self)
        switch self {
        case let .card(value): try tag.encode("card", forKey: .type); try value.encode(to: encoder)
        case let .row(value): try tag.encode("row", forKey: .type); try value.encode(to: encoder)
        case let .markdown(value): try tag.encode("markdown", forKey: .type); try value.encode(to: encoder)
        case let .contentRef(value): try tag.encode("content_ref", forKey: .type); try value.encode(to: encoder)
        case let .facts(value): try tag.encode("facts", forKey: .type); try value.encode(to: encoder)
        case let .checks(value): try tag.encode("checks", forKey: .type); try value.encode(to: encoder)
        case let .timeline(value): try tag.encode("timeline", forKey: .type); try value.encode(to: encoder)
        case let .form(value): try tag.encode("form", forKey: .type); try value.encode(to: encoder)
        case let .notice(value): try tag.encode("notice", forKey: .type); try value.encode(to: encoder)
        case let .actions(value): try tag.encode("actions", forKey: .type); try value.encode(to: encoder)
        }
    }
}

public struct Card: Codable, Equatable, Sendable {
    public let id: String; public let link: Link?; public let icon: Icon; public let eyebrow: Label?; public let title: String; public let subtitle: String?
    public let badges: [Label]; public let context: [ContextLine]; public let timestamps: [Stamp]; public let state: Label?; public let detail: Detail
    public let body: Markdown?; public let attachments: [Attachment]; public let quotes: [Quote]; public let children: [Card]
    public let facts: Facts?; public let checks: Checks?; public let actions: [Action]
    enum CodingKeys: String, CodingKey { case id, link, icon, eyebrow, title, subtitle, badges, context, timestamps, state, detail, body, attachments, quotes, children, facts, checks, actions }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id); link = try c.decodeIfPresent(Link.self, forKey: .link); icon = try c.decode(Icon.self, forKey: .icon)
        eyebrow = try c.decodeIfPresent(Label.self, forKey: .eyebrow); title = try c.decode(String.self, forKey: .title); subtitle = try c.decodeIfPresent(String.self, forKey: .subtitle)
        badges = try c.decodeIfPresent([Label].self, forKey: .badges) ?? []; context = try c.decodeIfPresent([ContextLine].self, forKey: .context) ?? []
        timestamps = try c.decodeIfPresent([Stamp].self, forKey: .timestamps) ?? []; state = try c.decodeIfPresent(Label.self, forKey: .state)
        detail = try c.decode(Detail.self, forKey: .detail); body = try c.decodeIfPresent(Markdown.self, forKey: .body)
        attachments = try c.decodeIfPresent([Attachment].self, forKey: .attachments) ?? []; quotes = try c.decodeIfPresent([Quote].self, forKey: .quotes) ?? []
        children = try c.decodeIfPresent([Card].self, forKey: .children) ?? []; facts = try c.decodeIfPresent(Facts.self, forKey: .facts)
        checks = try c.decodeIfPresent(Checks.self, forKey: .checks); actions = try c.decodeIfPresent([Action].self, forKey: .actions) ?? []
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id, forKey: .id); try c.encodeIfPresent(link, forKey: .link); try c.encode(icon, forKey: .icon)
        try c.encodeIfPresent(eyebrow, forKey: .eyebrow); try c.encode(title, forKey: .title); try c.encodeIfPresent(subtitle, forKey: .subtitle)
        if !badges.isEmpty { try c.encode(badges, forKey: .badges) }; if !context.isEmpty { try c.encode(context, forKey: .context) }
        if !timestamps.isEmpty { try c.encode(timestamps, forKey: .timestamps) }; try c.encodeIfPresent(state, forKey: .state)
        try c.encode(detail, forKey: .detail); try c.encodeIfPresent(body, forKey: .body)
        if !attachments.isEmpty { try c.encode(attachments, forKey: .attachments) }; if !quotes.isEmpty { try c.encode(quotes, forKey: .quotes) }
        if !children.isEmpty { try c.encode(children, forKey: .children) }; try c.encodeIfPresent(facts, forKey: .facts)
        try c.encodeIfPresent(checks, forKey: .checks); if !actions.isEmpty { try c.encode(actions, forKey: .actions) }
    }
}
public struct Row: Codable, Equatable, Sendable {
    public let id: String; public let link: Link?; public let icon: Icon; public let title: String; public let subtitle: String?; public let badges: [Label]
    public let trailing: Trailing?; public let depth: UInt8; public let struck: Bool; public let children: [Row]; public let actions: [Action]
    enum CodingKeys: String, CodingKey { case id, link, icon, title, subtitle, badges, trailing, depth, struck, children, actions }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id); link = try c.decodeIfPresent(Link.self, forKey: .link); icon = try c.decode(Icon.self, forKey: .icon)
        title = try c.decode(String.self, forKey: .title); subtitle = try c.decodeIfPresent(String.self, forKey: .subtitle)
        badges = try c.decodeIfPresent([Label].self, forKey: .badges) ?? []; trailing = try c.decodeIfPresent(Trailing.self, forKey: .trailing)
        depth = try c.decode(UInt8.self, forKey: .depth); struck = try c.decode(Bool.self, forKey: .struck)
        children = try c.decodeIfPresent([Row].self, forKey: .children) ?? []; actions = try c.decodeIfPresent([Action].self, forKey: .actions) ?? []
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id, forKey: .id); try c.encodeIfPresent(link, forKey: .link); try c.encode(icon, forKey: .icon)
        try c.encode(title, forKey: .title); try c.encodeIfPresent(subtitle, forKey: .subtitle)
        if !badges.isEmpty { try c.encode(badges, forKey: .badges) }; try c.encodeIfPresent(trailing, forKey: .trailing)
        try c.encode(depth, forKey: .depth); try c.encode(struck, forKey: .struck)
        if !children.isEmpty { try c.encode(children, forKey: .children) }; if !actions.isEmpty { try c.encode(actions, forKey: .actions) }
    }
}
public struct Markdown: Codable, Equatable, Sendable { public let text: String; public let truncated: Bool; public let more: Link?; public let commentable: Bool }
public struct ContentRef: Codable, Equatable, Sendable { public let kind: ContentKind; public let title: String; public let size: UInt64; public let digest: String?; public let fetch: String; public let commentable: Bool }
public struct Attachment: Codable, Equatable, Sendable {
    public let id: String; public let name: String; public let kind: AttachmentKind; public let size: UInt64; public let fetch: String; public let digest: String?
    enum CodingKeys: String, CodingKey { case id, name, kind, size, fetch, digest }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id); name = try c.decode(String.self, forKey: .name)
        kind = try c.decode(AttachmentKind.self, forKey: .kind); size = try c.decode(UInt64.self, forKey: .size)
        fetch = try c.decode(String.self, forKey: .fetch); digest = try c.decodeIfPresent(String.self, forKey: .digest)
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id, forKey: .id); try c.encode(name, forKey: .name); try c.encode(kind, forKey: .kind)
        try c.encode(size, forKey: .size); try c.encode(fetch, forKey: .fetch); try c.encodeIfPresent(digest, forKey: .digest)
    }
}
public struct Facts: Codable, Equatable, Sendable { public let items: [Fact] }
public struct Fact: Codable, Equatable, Sendable { public let label: String; public let value: String; public let tone: Tone?; public let link: Link? }
public struct Checks: Codable, Equatable, Sendable { public let items: [Check]; public let link: Link? }
public struct Check: Codable, Equatable, Sendable { public let name: String; public let state: CheckState }
public struct Timeline: Codable, Equatable, Sendable { public let entries: [TimelineEntry] }
public struct TimelineEntry: Codable, Equatable, Sendable { public let at: String; public let title: String; public let text: String?; public let tone: Tone? }
public struct Notice: Codable, Equatable, Sendable { public let text: String; public let tone: Tone; public let action: Action? }
public struct ActionBar: Codable, Equatable, Sendable { public let actions: [Action] }
public struct ContextLine: Codable, Equatable, Sendable { public let icon: Icon; public let text: String; public let link: Link?; public let chip: Label? }
public struct Stamp: Codable, Equatable, Sendable { public let role: String; public let at: String; public let format: TimeFormat }
public struct Quote: Codable, Equatable, Sendable { public let label: String; public let text: String; public let at: String? }

public enum Trailing: Codable, Equatable, Sendable {
    case count(value: UInt32), time(at: String, format: TimeFormat), text(text: String)
    private enum Keys: String, CodingKey { case kind, value, at, format, text }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "count": self = .count(value: try c.decode(UInt32.self, forKey: .value))
        case "time": self = .time(at: try c.decode(String.self, forKey: .at), format: try c.decode(TimeFormat.self, forKey: .format))
        case "text": self = .text(text: try c.decode(String.self, forKey: .text))
        case let kind: throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "Unknown trailing kind: \(kind)")
        }
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case let .count(value): try c.encode("count", forKey: .kind); try c.encode(value, forKey: .value)
        case let .time(at, format): try c.encode("time", forKey: .kind); try c.encode(at, forKey: .at); try c.encode(format, forKey: .format)
        case let .text(text): try c.encode("text", forKey: .kind); try c.encode(text, forKey: .text)
        }
    }
}

public struct Action: Codable, Equatable, Sendable {
    public let id: String?; public let label: String; public let style: Style; public let presentation: Presentation; public let options: [ActionOption]
    public let input: Input; public let attachments: Bool; public let comments: CommentSpec?; public let confirm: Confirm?; public let enabled: Bool
    public let disabledReason: String?; public let stamp: String?; public let key: String?; public let link: Link?
    enum CodingKeys: String, CodingKey { case id, label, style, presentation, options, input, attachments, comments, confirm, enabled; case disabledReason = "disabled_reason"; case stamp, key, link }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(String.self, forKey: .id); label = try c.decode(String.self, forKey: .label)
        style = try c.decode(Style.self, forKey: .style); presentation = try c.decode(Presentation.self, forKey: .presentation)
        options = try c.decodeIfPresent([ActionOption].self, forKey: .options) ?? []; input = try c.decode(Input.self, forKey: .input)
        attachments = try c.decode(Bool.self, forKey: .attachments); comments = try c.decodeIfPresent(CommentSpec.self, forKey: .comments)
        confirm = try c.decodeIfPresent(Confirm.self, forKey: .confirm); enabled = try c.decode(Bool.self, forKey: .enabled)
        disabledReason = try c.decodeIfPresent(String.self, forKey: .disabledReason); stamp = try c.decodeIfPresent(String.self, forKey: .stamp)
        key = try c.decodeIfPresent(String.self, forKey: .key); link = try c.decodeIfPresent(Link.self, forKey: .link)
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encodeIfPresent(id, forKey: .id); try c.encode(label, forKey: .label); try c.encode(style, forKey: .style)
        try c.encode(presentation, forKey: .presentation); if !options.isEmpty { try c.encode(options, forKey: .options) }
        try c.encode(input, forKey: .input); try c.encode(attachments, forKey: .attachments); try c.encodeIfPresent(comments, forKey: .comments)
        try c.encodeIfPresent(confirm, forKey: .confirm); try c.encode(enabled, forKey: .enabled); try c.encodeIfPresent(disabledReason, forKey: .disabledReason)
        try c.encodeIfPresent(stamp, forKey: .stamp); try c.encodeIfPresent(key, forKey: .key); try c.encodeIfPresent(link, forKey: .link)
    }
}
public struct ActionOption: Codable, Equatable, Sendable { public let id: String; public let label: String; public let description: String?; public let style: Style; public let input: Input; public let key: String? }
public struct CommentSpec: Codable, Equatable, Sendable { public let kind: CommentKind }
public struct Confirm: Codable, Equatable, Sendable { public let title: String; public let message: String; public let button: String }
public struct Condition: Codable, Equatable, Sendable {
    public let field: String
    public let equals: JSONValue?
    public let anyOf: [JSONValue]?
    enum CodingKeys: String, CodingKey { case field, equals; case anyOf = "any_of" }

    public init(field: String, equals: JSONValue) {
        self.field = field
        self.equals = equals
        self.anyOf = nil
    }

    public init(field: String, anyOf: [JSONValue]) {
        self.field = field
        self.equals = nil
        self.anyOf = anyOf
    }

    public func matches(_ value: JSONValue) -> Bool {
        equals == value || anyOf?.contains(value) == true
    }
}
public struct ChoiceItem: Codable, Equatable, Sendable { public let id: String; public let label: String; public let description: String? }
public struct Form: Codable, Equatable, Sendable { public let id: String; public let fields: [Field]; public let submit: Action }
public struct Field: Codable, Equatable, Sendable {
    public let id: String; public let label: String; public let kind: FieldKind; public let value: JSONValue; public let required: Bool; public let help: String?
    public let visibleWhen: Condition?; public let reloadOnChange: Bool
    enum CodingKeys: String, CodingKey { case id, label, kind, value, required, help; case visibleWhen = "visible_when"; case reloadOnChange = "reload_on_change" }
}

public enum Input: Codable, Equatable, Sendable {
    case none, text(placeholder: String?, required: Bool, multiline: Bool)
    private enum Keys: String, CodingKey { case kind, placeholder, required, multiline }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "none": self = .none
        case "text": self = .text(placeholder: try c.decodeIfPresent(String.self, forKey: .placeholder), required: try c.decode(Bool.self, forKey: .required), multiline: try c.decode(Bool.self, forKey: .multiline))
        case let kind: throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "Unknown input kind: \(kind)")
        }
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case .none: try c.encode("none", forKey: .kind)
        case let .text(placeholder, required, multiline): try c.encode("text", forKey: .kind); try c.encodeIfPresent(placeholder, forKey: .placeholder); try c.encode(required, forKey: .required); try c.encode(multiline, forKey: .multiline)
        }
    }
}

public enum FieldKind: Codable, Equatable, Sendable {
    case text(placeholder: String?), multiline(placeholder: String?), attachments, toggle, choice(items: [ChoiceItem]), picker(source: String), date, time, stepper(min: Int64, max: Int64), lines(placeholder: String?)
    private enum Keys: String, CodingKey { case kind, placeholder, items, source, min, max }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "text": self = .text(placeholder: try c.decodeIfPresent(String.self, forKey: .placeholder))
        case "multiline": self = .multiline(placeholder: try c.decodeIfPresent(String.self, forKey: .placeholder))
        case "attachments": self = .attachments
        case "toggle": self = .toggle
        case "choice": self = .choice(items: try c.decode([ChoiceItem].self, forKey: .items))
        case "picker": self = .picker(source: try c.decode(String.self, forKey: .source))
        case "date": self = .date
        case "time": self = .time
        case "stepper": self = .stepper(min: try c.decode(Int64.self, forKey: .min), max: try c.decode(Int64.self, forKey: .max))
        case "lines": self = .lines(placeholder: try c.decodeIfPresent(String.self, forKey: .placeholder))
        case let kind: throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "Unknown field kind: \(kind)")
        }
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case let .text(v): try c.encode("text", forKey: .kind); try c.encodeIfPresent(v, forKey: .placeholder)
        case let .multiline(v): try c.encode("multiline", forKey: .kind); try c.encodeIfPresent(v, forKey: .placeholder)
        case .attachments: try c.encode("attachments", forKey: .kind)
        case .toggle: try c.encode("toggle", forKey: .kind)
        case let .choice(v): try c.encode("choice", forKey: .kind); try c.encode(v, forKey: .items)
        case let .picker(v): try c.encode("picker", forKey: .kind); try c.encode(v, forKey: .source)
        case .date: try c.encode("date", forKey: .kind)
        case .time: try c.encode("time", forKey: .kind)
        case let .stepper(a, b): try c.encode("stepper", forKey: .kind); try c.encode(a, forKey: .min); try c.encode(b, forKey: .max)
        case let .lines(v): try c.encode("lines", forKey: .kind); try c.encodeIfPresent(v, forKey: .placeholder)
        }
    }
}

public enum Link: Codable, Equatable, Sendable {
    case view(key: String), url(url: String), local(local: LocalLink)
    private enum Keys: String, CodingKey { case kind, key, url, local }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "view": self = .view(key: try c.decode(String.self, forKey: .key))
        case "url": self = .url(url: try c.decode(String.self, forKey: .url))
        case "local": self = .local(local: try c.decode(LocalLink.self, forKey: .local))
        case let kind: throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "Unknown link kind: \(kind)")
        }
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case let .view(key): try c.encode("view", forKey: .kind); try c.encode(key, forKey: .key)
        case let .url(url): try c.encode("url", forKey: .kind); try c.encode(url, forKey: .url)
        case let .local(local): try c.encode("local", forKey: .kind); try c.encode(local, forKey: .local)
        }
    }
}
public enum LocalLink: Codable, Equatable, Sendable {
    case attach(session: String), reviewClone(repo: String, branch: String, pr: UInt64), openPath(path: String)
    private enum Keys: String, CodingKey { case kind, session, repo, branch, pr, path }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "attach": self = .attach(session: try c.decode(String.self, forKey: .session))
        case "review_clone": self = .reviewClone(repo: try c.decode(String.self, forKey: .repo), branch: try c.decode(String.self, forKey: .branch), pr: try c.decode(UInt64.self, forKey: .pr))
        case "open_path": self = .openPath(path: try c.decode(String.self, forKey: .path))
        case let kind: throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "Unknown local link kind: \(kind)")
        }
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case let .attach(v): try c.encode("attach", forKey: .kind); try c.encode(v, forKey: .session)
        case let .reviewClone(repo, branch, pr): try c.encode("review_clone", forKey: .kind); try c.encode(repo, forKey: .repo); try c.encode(branch, forKey: .branch); try c.encode(pr, forKey: .pr)
        case let .openPath(v): try c.encode("open_path", forKey: .kind); try c.encode(v, forKey: .path)
        }
    }
}

public enum JSONValue: Codable, Equatable, Sendable {
    case null, bool(Bool), integer(Int64), number(Double), string(String), array([JSONValue]), object([String: JSONValue])
    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let v = try? c.decode(Bool.self) { self = .bool(v) }
        else if let v = try? c.decode(Int64.self) { self = .integer(v) }
        else if let v = try? c.decode(Double.self) { self = .number(v) }
        else if let v = try? c.decode(String.self) { self = .string(v) }
        else if let v = try? c.decode([JSONValue].self) { self = .array(v) }
        else { self = .object(try c.decode([String: JSONValue].self)) }
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case let .bool(v): try c.encode(v)
        case let .integer(v): try c.encode(v)
        case let .number(v): try c.encode(v)
        case let .string(v): try c.encode(v)
        case let .array(v): try c.encode(v)
        case let .object(v): try c.encode(v)
        }
    }
}
