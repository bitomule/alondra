import AlondraContract
import Foundation

/// What the PR review screen reads from a `pull_request:` view, one field per section id.
/// A section the screen does not know is ignored.
public struct PRReview: Equatable, Sendable {
    public var title: String
    public var facts: [Fact] = []
    public var checks: [Check] = []
    public var attached: [Card] = []
    public var description: Markdown?
    public var diff: ContentRef?
    public var notices: [Notice]
    public var merge: WireAction?
    public var resolveConflicts: WireAction?
    public var ask: WireAction?
    public var requestChanges: WireAction?
    public var pageURL: URL?

    public init(document: ViewDocument) {
        title = document.title ?? String(localized: "Pull Request")
        notices = document.notices
        for section in document.sections {
            switch section.id {
            case "facts": facts = section.items.facts
            case "checks": checks = section.items.checks
            case "children": attached = section.items.cards
            case "description": description = section.items.markdown
            case "diff": diff = section.items.diff
            case "actions": sortActions(section.items.actions)
            default: break
            }
        }
    }

    private mutating func sortActions(_ actions: [WireAction]) {
        for action in actions {
            if case .url(let raw)? = action.link, action.id?.isEmpty ?? true {
                pageURL = PRReview.pageURL(raw)
                continue
            }
            switch action.id?.prefix(while: { $0 != ":" }) {
            case "merge": merge = action
            case "conflicts": resolveConflicts = action
            case "ask_pr": ask = action
            case "changes": requestChanges = action
            default: break
            }
        }
    }

    /// Only a page on the PR's own host is ever opened from a server link.
    public static func pageURL(_ raw: String) -> URL? {
        guard let url = URL(string: raw), url.scheme?.lowercased() == "https",
              let host = url.host?.lowercased(), host == "github.com" || host.hasSuffix(".github.com") else { return nil }
        return url
    }
}

private extension Array where Element == ViewItem {
    var facts: [Fact] {
        flatMap { item -> [Fact] in
            if case .facts(let value) = item { return value.items }
            return []
        }
    }

    var checks: [Check] {
        flatMap { item -> [Check] in
            if case .checks(let value) = item { return value.items }
            return []
        }
    }

    var cards: [Card] {
        compactMap { item -> Card? in
            if case .card(let value) = item { return value }
            return nil
        }
    }

    var markdown: Markdown? {
        for item in self { if case .markdown(let value) = item { return value } }
        return nil
    }

    var diff: ContentRef? {
        for item in self { if case .contentRef(let value) = item, value.kind == .diff { return value } }
        return nil
    }

    var actions: [WireAction] {
        flatMap { item -> [WireAction] in
            if case .actions(let bar) = item { return bar.actions }
            return []
        }
    }
}
