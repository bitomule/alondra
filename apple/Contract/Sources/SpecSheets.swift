import Foundation

/// A spec and a decision as the daemon words them for every client: never raw Gherkin. The app
/// only lays out what it receives.
public enum SheetChange: String, Codable, Equatable, Sendable, FallbackDecodable {
    case new, changed, removed, unchanged
    public static let fallback = SheetChange.unchanged
}

public struct SheetMark: Codable, Equatable, Sendable {
    public var change: SheetChange
    public var label: String
    public init(change: SheetChange, label: String) { self.change = change; self.label = label }
}

/// What a comment on a part of a sheet quotes to say which part it is about.
public struct SheetAnchor: Codable, Equatable, Sendable {
    public var id: String
    public var quote: String
    public init(id: String, quote: String) { self.id = id; self.quote = quote }
}

public struct SheetReference: Codable, Equatable, Sendable {
    public var title: String
    public var link: Link?
    public init(title: String, link: Link? = nil) { self.title = title; self.link = link }
    private enum CodingKeys: String, CodingKey { case title, link }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        title = try c.decode(String.self, forKey: .title)
        link = try c.decodeKnownIfPresent(Link.self, forKey: .link)
    }
}

public struct SheetReferences: Codable, Equatable, Sendable {
    public var label: String
    public var items: [SheetReference]
    public init(label: String, items: [SheetReference]) { self.label = label; self.items = items }
}

/// An image Alondra keeps, read like an attachment from `url`.
public struct SheetPicture: Codable, Equatable, Sendable {
    public var url: String
    public var width: UInt32
    public var height: UInt32
    public var bytes: UInt64
    public init(url: String, width: UInt32, height: UInt32, bytes: UInt64) {
        self.url = url; self.width = width; self.height = height; self.bytes = bytes
    }

    /// Width over height, which a thumbnail keeps; a picture of unknown size is drawn as a phone screen.
    public var aspectRatio: Double {
        width > 0 && height > 0 ? Double(width) / Double(height) : 9.0 / 19.5
    }
}

public enum SheetPartKind: String, Codable, Equatable, Sendable, FallbackDecodable {
    case diagram, mockup, other
    public static let fallback = SheetPartKind.other
}

/// A diagram or a mockup of a spec or of one of its scenarios, or a capture of the evidence.
public struct SheetPart: Codable, Equatable, Sendable, Identifiable {
    public var id: String
    public var kind: SheetPartKind
    public var name: String
    public var title: String
    /// What a screen reader says of it: the part and what it illustrates.
    public var alt: String
    public var mark: SheetMark?
    /// A diagram's Mermaid text.
    public var code: String?
    /// A mockup's image.
    public var picture: SheetPicture?
    /// Why it cannot be shown, when it cannot.
    public var unavailable: String?
    public var anchor: SheetAnchor?
    public var edit: Link?

    public init(id: String, kind: SheetPartKind, name: String, title: String, alt: String, mark: SheetMark? = nil, code: String? = nil,
                picture: SheetPicture? = nil, unavailable: String? = nil, anchor: SheetAnchor? = nil, edit: Link? = nil) {
        self.id = id; self.kind = kind; self.name = name; self.title = title; self.alt = alt; self.mark = mark; self.code = code
        self.picture = picture; self.unavailable = unavailable; self.anchor = anchor; self.edit = edit
    }

    private enum CodingKeys: String, CodingKey { case id, kind, name, title, alt, mark, code, picture, unavailable, anchor, edit }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id); kind = try c.decode(SheetPartKind.self, forKey: .kind)
        name = try c.decode(String.self, forKey: .name); title = try c.decode(String.self, forKey: .title); alt = try c.decode(String.self, forKey: .alt)
        mark = try c.decodeIfPresent(SheetMark.self, forKey: .mark); code = try c.decodeIfPresent(String.self, forKey: .code)
        picture = try c.decodeIfPresent(SheetPicture.self, forKey: .picture); unavailable = try c.decodeIfPresent(String.self, forKey: .unavailable)
        anchor = try c.decodeIfPresent(SheetAnchor.self, forKey: .anchor)
        edit = try c.decodeKnownIfPresent(Link.self, forKey: .edit)
    }
}

public struct SheetStep: Codable, Equatable, Sendable {
    public var text: String
    public var table: [[String]]
    public var doc: String?
    public init(text: String, table: [[String]] = [], doc: String? = nil) { self.text = text; self.table = table; self.doc = doc }
    private enum CodingKeys: String, CodingKey { case text, table, doc }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        text = try c.decode(String.self, forKey: .text)
        table = try c.decodeIfPresent([[String]].self, forKey: .table) ?? []
        doc = try c.decodeIfPresent(String.self, forKey: .doc)
    }
}

public enum SheetLaneKind: String, Codable, Equatable, Sendable, FallbackDecodable {
    case given, when, then, other
    public static let fallback = SheetLaneKind.other
}

/// The steps of one of the three lanes; `And` and `But` join the lane before them.
public struct SheetLane: Codable, Equatable, Sendable {
    public var kind: SheetLaneKind
    public var label: String
    public var steps: [SheetStep]
    public init(kind: SheetLaneKind, label: String, steps: [SheetStep]) { self.kind = kind; self.label = label; self.steps = steps }
}

public struct SheetTable: Codable, Equatable, Sendable {
    public var label: String
    /// The header row first.
    public var rows: [[String]]
    public init(label: String, rows: [[String]]) { self.label = label; self.rows = rows }
}

/// A line of a result that carries its own icon and tone, so it never depends on colour alone.
public struct SheetLine: Codable, Equatable, Sendable {
    public var icon: Icon
    public var tone: Tone
    public var text: String
    public var detail: String?
    public init(icon: Icon, tone: Tone, text: String, detail: String? = nil) { self.icon = icon; self.tone = tone; self.text = text; self.detail = detail }
}

/// A validator's verdict: always a judgment, never a fact a test showed.
public struct SheetVerdict: Codable, Equatable, Sendable {
    public var line: SheetLine
    public var reasoning: String?
    public var nearestLabel: String?
    public var nearestWrong: String?
    public var evidence: [SheetPart]
    public init(line: SheetLine, reasoning: String? = nil, nearestLabel: String? = nil, nearestWrong: String? = nil, evidence: [SheetPart] = []) {
        self.line = line; self.reasoning = reasoning; self.nearestLabel = nearestLabel; self.nearestWrong = nearestWrong; self.evidence = evidence
    }
    private enum CodingKeys: String, CodingKey { case line, reasoning, evidence; case nearestLabel = "nearest_label", nearestWrong = "nearest_wrong" }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        line = try c.decode(SheetLine.self, forKey: .line); reasoning = try c.decodeIfPresent(String.self, forKey: .reasoning)
        nearestLabel = try c.decodeIfPresent(String.self, forKey: .nearestLabel); nearestWrong = try c.decodeIfPresent(String.self, forKey: .nearestWrong)
        evidence = try c.decodeIfPresent([SheetPart].self, forKey: .evidence) ?? []
    }
}

/// How a scenario of a delivery was validated.
public struct SheetResult: Codable, Equatable, Sendable {
    public var state: SheetLine
    public var checks: [SheetLine]
    public var tests: [SheetLine]
    public var verdicts: [SheetVerdict]
    public init(state: SheetLine, checks: [SheetLine] = [], tests: [SheetLine] = [], verdicts: [SheetVerdict] = []) {
        self.state = state; self.checks = checks; self.tests = tests; self.verdicts = verdicts
    }
    private enum CodingKeys: String, CodingKey { case state, checks, tests, verdicts }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        state = try c.decode(SheetLine.self, forKey: .state)
        checks = try c.decodeIfPresent([SheetLine].self, forKey: .checks) ?? []
        tests = try c.decodeIfPresent([SheetLine].self, forKey: .tests) ?? []
        verdicts = try c.decodeIfPresent([SheetVerdict].self, forKey: .verdicts) ?? []
    }
}

public struct SheetScenario: Codable, Equatable, Sendable, Identifiable {
    public var id: String
    public var title: String
    /// «test», «judgment on captures».
    public var kind: String
    public var mark: SheetMark?
    /// Drawn closed until the reader opens it.
    public var collapsed: Bool
    public var lanes: [SheetLane]
    public var examples: SheetTable?
    public var parts: [SheetPart]
    public var result: SheetResult?
    public var anchor: SheetAnchor?
    public var edit: Link?
    public var add: [SheetReference]

    public init(id: String, title: String, kind: String, mark: SheetMark? = nil, collapsed: Bool = false, lanes: [SheetLane], examples: SheetTable? = nil,
                parts: [SheetPart] = [], result: SheetResult? = nil, anchor: SheetAnchor? = nil, edit: Link? = nil, add: [SheetReference] = []) {
        self.id = id; self.title = title; self.kind = kind; self.mark = mark; self.collapsed = collapsed; self.lanes = lanes; self.examples = examples
        self.parts = parts; self.result = result; self.anchor = anchor; self.edit = edit; self.add = add
    }

    private enum CodingKeys: String, CodingKey { case id, title, kind, mark, collapsed, lanes, examples, parts, result, anchor, edit, add }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id); title = try c.decode(String.self, forKey: .title); kind = try c.decode(String.self, forKey: .kind)
        mark = try c.decodeIfPresent(SheetMark.self, forKey: .mark); collapsed = try c.decodeIfPresent(Bool.self, forKey: .collapsed) ?? false
        lanes = try c.decode([SheetLane].self, forKey: .lanes); examples = try c.decodeIfPresent(SheetTable.self, forKey: .examples)
        parts = try c.decodeIfPresent([SheetPart].self, forKey: .parts) ?? []
        result = try c.decodeIfPresent(SheetResult.self, forKey: .result); anchor = try c.decodeIfPresent(SheetAnchor.self, forKey: .anchor)
        edit = try c.decodeKnownIfPresent(Link.self, forKey: .edit); add = try c.decodeIfPresent([SheetReference].self, forKey: .add) ?? []
    }
}

public struct SheetBackground: Codable, Equatable, Sendable {
    public var label: String
    public var lanes: [SheetLane]
    public init(label: String, lanes: [SheetLane]) { self.label = label; self.lanes = lanes }
}

/// The scenarios a change leaves as they are, folded into one line.
public struct SheetUnchanged: Codable, Equatable, Sendable {
    public var label: String
    public var scenarios: [SheetScenario]
    public init(label: String, scenarios: [SheetScenario]) { self.label = label; self.scenarios = scenarios }
}

/// One spec.
public struct SpecSheet: Codable, Equatable, Sendable, Identifiable {
    public var id: String
    public var spec: String
    public var title: String
    public var mark: SheetMark?
    public var explanation: String
    public var decisions: SheetReferences?
    public var parts: [SheetPart]
    public var background: SheetBackground?
    public var scenarios: [SheetScenario]
    public var unchanged: SheetUnchanged?
    /// Said when it has no scenario.
    public var empty: String?
    public var anchor: SheetAnchor?
    public var edit: Link?
    public var add: [SheetReference]
    public var actions: [Action]

    public init(id: String, spec: String, title: String, mark: SheetMark? = nil, explanation: String = "", decisions: SheetReferences? = nil, parts: [SheetPart] = [],
                background: SheetBackground? = nil, scenarios: [SheetScenario], unchanged: SheetUnchanged? = nil, empty: String? = nil, anchor: SheetAnchor? = nil,
                edit: Link? = nil, add: [SheetReference] = [], actions: [Action] = []) {
        self.id = id; self.spec = spec; self.title = title; self.mark = mark; self.explanation = explanation; self.decisions = decisions; self.parts = parts
        self.background = background; self.scenarios = scenarios; self.unchanged = unchanged; self.empty = empty; self.anchor = anchor; self.edit = edit
        self.add = add; self.actions = actions
    }

    private enum CodingKeys: String, CodingKey { case id, spec, title, mark, explanation, decisions, parts, background, scenarios, unchanged, empty, anchor, edit, add, actions }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id); spec = try c.decode(String.self, forKey: .spec); title = try c.decode(String.self, forKey: .title)
        mark = try c.decodeIfPresent(SheetMark.self, forKey: .mark); explanation = try c.decodeIfPresent(String.self, forKey: .explanation) ?? ""
        decisions = try c.decodeIfPresent(SheetReferences.self, forKey: .decisions); parts = try c.decodeIfPresent([SheetPart].self, forKey: .parts) ?? []
        background = try c.decodeIfPresent(SheetBackground.self, forKey: .background); scenarios = try c.decode([SheetScenario].self, forKey: .scenarios)
        unchanged = try c.decodeIfPresent(SheetUnchanged.self, forKey: .unchanged); empty = try c.decodeIfPresent(String.self, forKey: .empty)
        anchor = try c.decodeIfPresent(SheetAnchor.self, forKey: .anchor); edit = try c.decodeKnownIfPresent(Link.self, forKey: .edit)
        add = try c.decodeIfPresent([SheetReference].self, forKey: .add) ?? []; actions = try c.decodeOfferedActions(forKey: .actions)
    }
}

public struct SheetSection: Codable, Equatable, Sendable {
    public var label: String
    public var text: String
    public init(label: String, text: String) { self.label = label; self.text = text }
}

/// A lasting decision.
public struct DecisionSheet: Codable, Equatable, Sendable, Identifiable {
    public var id: String
    public var decision: String
    public var title: String
    public var status: String
    public var mark: SheetMark?
    /// Context, decision, consequences and how it is checked.
    public var sections: [SheetSection]
    public var supersedes: SheetReferences?
    public var supersededBy: SheetReferences?
    public var anchor: SheetAnchor?
    public var edit: Link?

    public init(id: String, decision: String, title: String, status: String, mark: SheetMark? = nil, sections: [SheetSection], supersedes: SheetReferences? = nil,
                supersededBy: SheetReferences? = nil, anchor: SheetAnchor? = nil, edit: Link? = nil) {
        self.id = id; self.decision = decision; self.title = title; self.status = status; self.mark = mark; self.sections = sections
        self.supersedes = supersedes; self.supersededBy = supersededBy; self.anchor = anchor; self.edit = edit
    }

    private enum CodingKeys: String, CodingKey { case id, decision, title, status, mark, sections, supersedes, anchor, edit; case supersededBy = "superseded_by" }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id); decision = try c.decode(String.self, forKey: .decision); title = try c.decode(String.self, forKey: .title)
        status = try c.decode(String.self, forKey: .status); mark = try c.decodeIfPresent(SheetMark.self, forKey: .mark)
        sections = try c.decode([SheetSection].self, forKey: .sections); supersedes = try c.decodeIfPresent(SheetReferences.self, forKey: .supersedes)
        supersededBy = try c.decodeIfPresent(SheetReferences.self, forKey: .supersededBy); anchor = try c.decodeIfPresent(SheetAnchor.self, forKey: .anchor)
        edit = try c.decodeKnownIfPresent(Link.self, forKey: .edit)
    }
}

public extension SpecSheet {
    /// Every scenario it holds, the folded ones included.
    var allScenarios: [SheetScenario] { scenarios + (unchanged?.scenarios ?? []) }
}

public extension SheetPart { var anchors: [SheetAnchor] { anchor.map { [$0] } ?? [] } }

public extension SheetScenario {
    /// The anchors a comment can be made on, in the order the scenario shows them.
    var anchors: [SheetAnchor] {
        (anchor.map { [$0] } ?? []) + parts.flatMap(\.anchors)
            + (result?.verdicts.flatMap { $0.evidence.flatMap(\.anchors) } ?? [])
    }
}

public extension SpecSheet {
    var anchors: [SheetAnchor] {
        (anchor.map { [$0] } ?? []) + parts.flatMap(\.anchors) + allScenarios.flatMap(\.anchors)
    }
}

public extension DecisionSheet { var anchors: [SheetAnchor] { anchor.map { [$0] } ?? [] } }

public extension ViewDocument {
    /// Every part of a sheet in the view a comment can be anchored to.
    var sheetAnchors: [SheetAnchor] {
        sections.flatMap(\.items).flatMap { item -> [SheetAnchor] in
            switch item {
            case let .specSheet(sheet): sheet.anchors
            case let .decisionSheet(sheet): sheet.anchors
            default: []
            }
        }
    }
}
