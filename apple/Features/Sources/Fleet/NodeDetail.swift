import AlondraContract
import Foundation

/// What the node screen reads from a `node:` view, one field per section id. A section the screen
/// does not know is ignored.
public struct NodeDetail: Equatable, Sendable {
    public struct Activity: Equatable, Sendable, Identifiable {
        public var id: String
        public var title: String
        public var detail: String?
        public var since: String?
        public var warning: String?
    }

    public var title: String
    public var state: String?
    public var stateTone: Tone?
    public var folder: String?
    public var since: String?
    public var waiting: [Card] = []
    public var tasks: [Row] = []
    public var needs: Markdown?
    public var summary: Markdown?
    /// Why the orchestrator's last run failed, in the user's words.
    public var failure: Markdown?
    public var lastMessage: Markdown?
    public var lastMessageAt: String?
    public var lastMessageFormat = TimeFormat.relative
    public var now: [Activity] = []
    public var background: [Activity] = []
    public var message: WireAction?
    public var interrupt: WireAction?
    public var close: WireAction?
    public var orchestratorActions: [WireAction] = []
    public var facts: [Fact] = []
    /// What the node mod reports: reachability, the tool it runs, its last turn, context and cost.
    public var readings: [Fact] = []

    public init(document: ViewDocument) {
        title = document.title ?? ""
        for section in document.sections {
            let items = section.items
            switch section.id {
            case "header":
                facts = items.facts
                state = facts.first?.value
                stateTone = facts.first?.tone
                folder = facts.dropFirst().first { $0.value.hasPrefix("/") }?.value
            case "readings": readings = items.facts
            case "since":
                since = Self.rows(items).compactMap(\.time).first?.at
            case "waiting": waiting = items.cards
            case "tasks": tasks = Self.rows(items)
            case "needs": needs = Self.markdown(items)
            case "summary": summary = Self.markdown(items)
            case "failure": failure = Self.markdown(items)
            case "last_message":
                lastMessage = Self.markdown(items)
                if let time = Self.rows(items).compactMap(\.time).first {
                    lastMessageAt = time.at
                    lastMessageFormat = time.format
                }
            case "now": now = Self.rows(items).map(Activity.init(row:))
            case "background": background = Self.rows(items).map(Activity.init(row:))
            case "actions": sortActions(items.actions)
            default: break
            }
        }
    }

    /// The node as it is after what was just sent: an orchestrator decision taken here leaves the
    /// screen and the state says what happens next.
    public func hiding(_ optimistic: Optimistic) -> NodeDetail {
        guard let taken = orchestratorActions.first(where: { optimistic.hides($0.id) }) else { return self }
        var detail = self
        detail.orchestratorActions = []
        detail.state = switch taken.command {
        case "orch_retry": String(localized: "Retrying")
        case "orch_stop": String(localized: "Stopping")
        default: String(localized: "Continuing")
        }
        return detail
    }

    private mutating func sortActions(_ actions: [WireAction]) {
        for action in actions {
            switch action.id?.prefix(while: { $0 != ":" }) {
            case "msg": message = action
            case "interrupt": interrupt = action
            case "close": close = action
            case "orch_retry", "orch_ack", "orch_stop": orchestratorActions.append(action)
            default: break
            }
        }
    }

    private static func rows(_ items: [ViewItem]) -> [Row] { items.rows }

    private static func markdown(_ items: [ViewItem]) -> Markdown? { items.markdown }
}

private extension Array where Element == ViewItem {
    var facts: [Fact] {
        flatMap { item -> [Fact] in
            if case .facts(let value) = item { return value.items }
            return []
        }
    }

    var cards: [Card] {
        compactMap { item -> Card? in
            if case .card(let value) = item { return value }
            return nil
        }
    }

    var rows: [Row] {
        compactMap { item -> Row? in
            if case .row(let value) = item { return value }
            return nil
        }
    }

    var markdown: Markdown? {
        for item in self { if case .markdown(let value) = item { return value } }
        return nil
    }

    var actions: [WireAction] {
        flatMap { item -> [WireAction] in
            if case .actions(let bar) = item { return bar.actions }
            return []
        }
    }
}

extension NodeDetail.Activity {
    init(row: Row) {
        id = row.id
        title = row.title
        detail = row.subtitle
        since = row.time?.at
        warning = row.badges.first?.text
    }
}

private extension Row {
    var time: (at: String, format: TimeFormat)? {
        if case .time(let at, let format)? = trailing { (at, format) } else { nil }
    }
}
