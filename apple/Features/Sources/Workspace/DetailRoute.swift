import AlondraContract
import ComposableArchitecture
import Foundation

/// A screen pushed on top of a tab, whatever the tab. Each case is one feature with its own
/// reducer; `legacy` is the generic renderer that screens use until they are rebuilt, and it
/// goes when the last screen has moved.
@Reducer(state: .equatable)
public enum DetailRoute {
    case legacy(ViewsScreenFeature)
    case taskDetail(TaskDetailFeature)
    case requestDetail(RequestDetailFeature)
    case schedules(SchedulesFeature)
    case scheduleEditor(ScheduleEditorFeature)
    case review(ReviewFeature)
    case prReview(PRReviewFeature)
    case nodeDetail(NodeDetailFeature)
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
        case .card where key.pullRequestID != nil:
            self = .prReview(PRReviewFeature.State(key: .pullRequest(key.pullRequestID ?? "")))
        case .card, .document: self = .review(ReviewFeature.State(key: key))
        case .task: self = .taskDetail(TaskDetailFeature.State(key: key))
        case .request: self = .requestDetail(RequestDetailFeature.State(key: key))
        case .schedules: self = .schedules(SchedulesFeature.State())
        case .schedule: self = .scheduleEditor(ScheduleEditorFeature.State(key: key))
        case .pullRequest: self = .prReview(PRReviewFeature.State(key: key))
        case .node: self = .nodeDetail(NodeDetailFeature.State(key: key))
        case nil: return nil
        default: self = .legacy(ViewsScreenFeature.State(key: key, owner: owner))
        }
    }

    var key: ViewKey {
        switch self {
        case .legacy(let state): state.key
        case .review(let state): state.screen.key
        case .taskDetail(let state): state.screen.key
        case .requestDetail(let state): state.screen.key
        case .schedules(let state): state.screen.key
        case .scheduleEditor(let state): state.screen.key
        case .prReview(let state): state.screen.key
        case .nodeDetail(let state): state.screen.key
        }
    }
}

public extension DetailRoute.Action {
    /// What the screen on top of a stack asks of the app, whichever screen it is.
    var delegate: ScreenDelegate? {
        switch self {
        case .legacy(.action(.delegate(let outcome))): ScreenDelegate(outcome)
        case .review(.delegate(.open(let key))): .open(key)
        case .review(.actions(.delegate(let outcome))): ScreenDelegate(outcome)
        case .taskDetail(.delegate(let delegate)),
             .requestDetail(.delegate(let delegate)),
             .schedules(.delegate(let delegate)),
             .scheduleEditor(.delegate(let delegate)),
             .prReview(.delegate(let delegate)),
             .nodeDetail(.delegate(let delegate)):
            delegate
        default: nil
        }
    }
}

private extension ViewKey {
    /// A pull request's card is keyed by the PR itself (`card:<repo>#12`); it opens the PR review.
    var pullRequestID: String? {
        guard rawValue.hasPrefix("card:") else { return nil }
        let id = String(rawValue.dropFirst("card:".count))
        return id.contains("#") ? id : nil
    }
}
