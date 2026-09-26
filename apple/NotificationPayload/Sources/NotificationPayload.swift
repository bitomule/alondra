import Foundation

/// The part of an APNs payload used by both the app and the service extension.
/// It deliberately has no dependency on application state, networking, or TCA.
public enum NotificationPayload {
    /// Context label echoed by APNs. It never carries credentials or signing material.
    public struct Binding: Hashable, Sendable {
        public let value: String

        public init?(_ value: String) {
            guard value.count == 43,
                  value.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95 }),
                  let bytes = Data(base64Encoded: value.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/") + "="),
                  bytes.count == 32,
                  bytes.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "") == value
            else { return nil }
            self.value = value
        }

        public init?(userInfo: [AnyHashable: Any]) {
            guard let alondra = userInfo["alondra"] as? [String: Any],
                  let value = alondra["push_binding"] as? String else { return nil }
            self.init(value)
        }
    }

    /// An inbox item cannot be routed or acted on without its APNs context label.
    public struct BoundItem: Sendable {
        public let item: Item
        public let binding: Binding

        public init?(userInfo: [AnyHashable: Any]) {
            guard let item = Item(userInfo: userInfo), let binding = Binding(userInfo: userInfo) else { return nil }
            self.item = item
            self.binding = binding
        }

        public func matches(_ currentBinding: Binding?) -> Bool { binding == currentBinding }
    }

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

        public struct QuestionIdentity: Equatable, Sendable {
            public let binding: Binding
            public let id: UInt64
        }

        public static func questionID(_ id: UInt64, binding: Binding) -> String {
            questionPrefix + binding.value + "_" + String(id)
        }

        public static func questionID(from identifier: String) -> QuestionIdentity? {
            guard identifier.hasPrefix(questionPrefix) else { return nil }
            let suffix = identifier.dropFirst(questionPrefix.count)
            guard suffix.count > 44,
                  let binding = Binding(String(suffix.prefix(43))),
                  suffix.dropFirst(43).first == "_",
                  let id = UInt64(suffix.dropFirst(44)) else { return nil }
            return QuestionIdentity(binding: binding, id: id)
        }

        public struct Retention: Equatable, Sendable {
            public let identifiers: Set<String>
            public let dynamicOrder: [String]
        }

        /// Bound dynamic categories globally. An arriving push does not prove its
        /// pairing is the active one, so an old binding never purges a newer one.
        public static func retainedCategoryIDs(
            from identifiers: [String],
            previousOrder: [String],
            incoming: String?,
            limit: Int
        ) -> Retention {
            let known = Set(identifiers.filter { questionID(from: $0) != nil })
            var seen = Set<String>()
            let ordered = previousOrder.filter { known.contains($0) && seen.insert($0).inserted }
            let missing = known.subtracting(seen).sorted()
            var dynamic = missing + ordered
            if let incoming, questionID(from: incoming) != nil {
                dynamic.removeAll { $0 == incoming }
                dynamic.append(incoming)
            }
            dynamic = Array(dynamic.suffix(max(0, limit)))
            let other = identifiers.filter { questionID(from: $0) == nil }
            return Retention(identifiers: Set(other).union(dynamic), dynamicOrder: dynamic)
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
