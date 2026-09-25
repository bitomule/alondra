import Foundation

/// The part of an APNs payload used by both the app and the service extension.
/// It deliberately has no dependency on application state, networking, or TCA.
public enum NotificationPayload {
    public struct Item: Hashable, Sendable {
        public enum Kind: String, Sendable {
            case question, report, pr
        }

        public let kind: Kind
        public let id: UInt64

        public var inboxID: String {
            switch kind {
            case .question: "q\(id)"
            case .report: "r\(id)"
            case .pr: "p\(id)"
            }
        }

        public init?(userInfo: [AnyHashable: Any]) {
            guard let alondra = userInfo["alondra"] as? [String: Any],
                  let kind = (alondra["kind"] as? String).flatMap(Kind.init(rawValue:)),
                  let number = alondra["id"] as? NSNumber,
                  let id = UInt64(number.stringValue) else { return nil }
            self.kind = kind
            self.id = id
        }
    }

    public struct Question: Sendable {
        public static let maxOptions = 4
        private static let dynamicOrigins: Set<String> = ["ask", "ask_user_question", "elicitation"]

        public let id: UInt64
        public let options: [String]

        public init?(userInfo: [AnyHashable: Any]) {
            guard let item = Item(userInfo: userInfo), item.kind == .question,
                  let alondra = userInfo["alondra"] as? [String: Any],
                  let origin = alondra["origin"] as? String, Self.dynamicOrigins.contains(origin),
                  let raw = alondra["options"] as? [String] else { return nil }
            var seen = Set<String>()
            let options = raw
                .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
                .filter { !$0.isEmpty && seen.insert($0).inserted }
                .prefix(Self.maxOptions)
            guard !options.isEmpty else { return nil }
            id = item.id
            self.options = Array(options)
        }
    }

    public enum Category {
        public static let question = "ALONDRA_QUESTION"
        public static let permission = "ALONDRA_PERMISSION"
        public static let plan = "ALONDRA_PLAN"
        public static let read = "ALONDRA_READ"
        public static let questionPrefix = "ALONDRA_Q_"

        public static func questionID(_ id: UInt64) -> String { questionPrefix + String(id) }

        public static func questionID(from identifier: String) -> UInt64? {
            guard identifier.hasPrefix(questionPrefix) else { return nil }
            return UInt64(identifier.dropFirst(questionPrefix.count))
        }
    }

    public enum Action {
        public static let reply = "REPLY"
        public static let moreContext = "MORE_CONTEXT"
        public static let allow = "ALLOW"
        public static let deny = "DENY"
        public static let approve = "APPROVE"
        public static let review = "REVIEW"
        public static let markRead = "READ"
        public static let optionPrefix = "OPT:"

        public static func option(_ label: String) -> String { optionPrefix + label }
    }
}
