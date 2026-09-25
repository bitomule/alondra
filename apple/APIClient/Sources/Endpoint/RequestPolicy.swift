import Foundation

public enum RequestPolicy: Sendable {
    case normal
    case merge
    case longPoll
    case upload

    public var timeout: TimeInterval {
        switch self {
        case .normal: 15
        case .merge: 95
        case .longPoll: 70
        case .upload: 120
        }
    }
}
