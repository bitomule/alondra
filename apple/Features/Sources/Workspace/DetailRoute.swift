import AlondraContract
import ComposableArchitecture
import Foundation

/// A screen pushed on top of a tab, whatever the tab. Each case is one feature with its own
/// reducer; `legacy` is the generic renderer that screens use until they are rebuilt, and it
/// goes when the last screen has moved.
@Reducer(state: .equatable)
public enum DetailRoute {
    case legacy(ViewsScreenFeature)
    case review(ReviewFeature)
}

public extension DetailRoute {
    /// The screen a server link opens. A key outside this table opens nothing, so a view the app
    /// does not know shows up as a failing test instead of an unexplained generic screen.
    enum Kind: Equatable, Sendable, CaseIterable {
        case card, document, pullRequest, task, request, node, schedules, schedule, tasksPage

        public init?(key: ViewKey) {
            let raw = key.rawValue
            switch raw {
            case "schedules": self = .schedules
            case _ where raw.hasPrefix("card:"): self = .card
            case _ where raw.hasPrefix("document:"): self = .document
            case _ where raw.hasPrefix("pull_request:"): self = .pullRequest
            case _ where raw.hasPrefix("task:"): self = .task
            case _ where raw.hasPrefix("request:"): self = .request
            case _ where raw.hasPrefix("node:"): self = .node
            case _ where raw.hasPrefix("schedule:"): self = .schedule
            case _ where raw.hasPrefix("tasks?"): self = .tasksPage
            default: return nil
            }
        }
    }
}

public extension DetailRoute.State {
    init?(link: AlondraContract.Link, owner: String) {
        guard case .view(let key) = link else { return nil }
        self.init(key: ViewKey(rawValue: key), owner: owner)
    }

    init?(key: ViewKey, owner: String) {
        switch DetailRoute.Kind(key: key) {
        case .card, .document: self = .review(ReviewFeature.State(key: key))
        case .some: self = .legacy(ViewsScreenFeature.State(key: key, owner: owner))
        case nil: return nil
        }
    }

    var key: ViewKey {
        switch self {
        case .legacy(let state): state.key
        case .review(let state): state.screen.key
        }
    }
}
