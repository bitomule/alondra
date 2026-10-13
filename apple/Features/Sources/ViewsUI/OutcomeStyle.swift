import AlondraContract
import AlondraFeed
import AlondraTransport
import Foundation

/// How a reply from the daemon reads and looks, the same on iPhone and Mac: the server words it,
/// this only decides whether it is a notice, a question or an error. `result` is the server's
/// typed outcome; a daemon that predates it is read through `message` and `stamp`.
public enum OutcomeStyle: Equatable, Sendable {
    case success(String)
    case failure(String)
    case ask(ActionFeature.ChangedNotice)

    public static var transportFailure: String { SendFailure.unreached.message }

    public init(_ reply: ActionResponse) {
        if let result = reply.result {
            switch result.kind {
            case .done, .started:
                self = .success(result.title)
            case .refused, .failed:
                self = .failure([result.title, result.body].compactMap { $0 }.joined(separator: " "))
            case .changed:
                guard let next = result.next, let id = next.id else {
                    self = .failure(result.title)
                    return
                }
                self = .ask(.init(title: result.title, body: result.body, actionId: id, button: next.label, resolvesCard: next.resolvesCard))
            case .other:
                self = reply.outcome == .applied
                    ? .success(result.title)
                    : .failure([result.title, result.body].compactMap { $0 }.joined(separator: " "))
            }
            return
        }
        switch reply.outcome {
        case .applied:
            self = .success(reply.message ?? String(localized: "Action completed"))
        case .changed:
            guard let confirm = reply.confirm else {
                self = .failure(reply.message ?? String(localized: "Action failed"))
                return
            }
            self = .ask(.init(title: reply.message ?? String(localized: "This changed"), body: nil,
                              actionId: confirm.actionId, button: confirm.button))
        case .noLongerAvailable, .rejected:
            self = .failure(reply.message ?? String(localized: "Action failed"))
        }
    }
}

/// Why an action got no answer from the daemon. Whether it could have reached the Mac decides the
/// words, and whether pressing again must reuse the same command so the Mac does it only once.
public enum SendFailure: Equatable, Sendable {
    /// Nothing left this device, or the Mac proved it never got it.
    case unreached
    /// The Mac answered, with an error of its own: nothing was done.
    case answered(String)
    /// It went out and no answer came back: it may have been done.
    case unsure

    public var message: String {
        switch self {
        case .unreached: String(localized: "Couldn’t reach your Mac. Nothing was sent; try again.")
        case .answered(let message): message
        case .unsure: String(localized: "Your Mac didn’t confirm it. It may have arrived: try again and it won’t be done twice.")
        }
    }

    public init(preparation error: any Error) {
        let failure = Self(error)
        self = failure == .unsure || error is ViewActionIntentError || error is FeedError
            ? .answered(String(localized: "Alondra couldn’t prepare this request."))
            : failure
    }

    public init(_ error: any Error) {
        switch error {
        case let error as APIError:
            switch error {
            case .unauthorized, .forbidden:
                self = .answered(String(localized: "This device is no longer approved. Approve it again from the Mac."))
            case .conflict:
                self = .answered(String(localized: "This changed while you were looking. It’s updated now; try again."))
            case .server, .unexpected:
                self = .answered(String(localized: "Your Mac couldn’t do it. Nothing changed; try again."))
            case .unavailable:
                self = .unsure
            }
        case let error as ActionDeliveryError:
            switch error {
            case .dataReset:
                self = .answered(String(localized: "Your Mac’s data was reset, so this answer no longer applies."))
            case .answeredBefore:
                self = .answered(String(localized: "Your Mac already had this answer."))
            }
        case is ViewActionIntentError, is FeedError:
            self = .answered(String(localized: "Alondra couldn’t save this answer on this device."))
        case DirectSessionError.deviceRevoked:
            self = .answered(String(localized: "This device is no longer approved. Approve it again from the Mac."))
        case is DirectRouteError:
            self = .unreached
        case let error where LoopbackSession.neverSent(error):
            self = .unreached
        default:
            self = .unsure
        }
    }
}

public extension WireAction {
    /// The colour a button is painted in, on every card and every client: red only for an action the
    /// server styles destructive, whatever its role says.
    var paintedRole: ActionRole {
        if style == .destructive { return .risk }
        return role == .advance || style == .primary ? .advance : .neutral
    }
}

public extension Card {
    /// The buttons a card shows: an action the server marks unusable is never painted, so no card
    /// waits greyed out.
    var usableActions: [WireAction] { actions.filter(\.enabled) }
    var usableMore: [WireAction] { more.filter(\.enabled) }
}
