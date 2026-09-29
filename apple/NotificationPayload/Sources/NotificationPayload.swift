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

    /// A v2 push names the server's inbox item and, for an actionable item, the epoch and
    /// revision its action is decided against (docs/domain-v2-apple-client-contracts.md).
    public struct Item: Hashable, Sendable {
        public enum Kind: String, Sendable {
            case question, actionRequest = "action_request", information, delivery, pullRequest = "pull_request"
        }

        public let kind: Kind
        public let itemID: String
        public let interactionID: String?
        public let epoch: String?
        public let expectedRevision: UInt64?

        public var inboxID: String { itemID }

        public init?(userInfo: [AnyHashable: Any]) {
            guard let alondra = userInfo["alondra"] as? [String: Any],
                  (alondra["protocol_version"] as? NSNumber)?.intValue == 2,
                  let kind = (alondra["kind"] as? String).flatMap(Kind.init(rawValue:)),
                  let itemID = alondra["item_id"] as? String, !itemID.isEmpty, itemID.utf8.count <= 512
            else { return nil }
            self.kind = kind
            self.itemID = itemID
            interactionID = (alondra["interaction_id"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            epoch = (alondra["data_epoch"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            expectedRevision = (alondra["expected_revision"] as? NSNumber).flatMap { UInt64(exactly: $0.int64Value) }
        }
    }

    /// The server's options for a question, offered as notification buttons by option ID.
    public struct Question: Sendable {
        public static let maxOptions = 4

        public struct Option: Hashable, Sendable {
            public let id: String
            public let label: String
            /// The option's text input, for the reply action.
            public let textInput: String?
        }

        public let id: String
        public let options: [Option]
        /// The option a typed notification reply answers with, when the server names one.
        public let replyOption: Option?

        public init?(userInfo: [AnyHashable: Any]) {
            guard let item = Item(userInfo: userInfo), item.kind == .question || item.kind == .actionRequest,
                  let id = item.interactionID, Category.isSafe(id),
                  let alondra = userInfo["alondra"] as? [String: Any],
                  let raw = alondra["options"] as? [[String: Any]] else { return nil }
            var seen = Set<String>()
            let all: [Option] = raw.compactMap { entry in
                guard let id = entry["id"] as? String, Category.isSafe(id),
                      let label = (entry["label"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines),
                      !label.isEmpty, seen.insert(id).inserted else { return nil }
                return Option(id: id, label: label, textInput: (entry["text_input"] as? String).flatMap { $0.isEmpty ? nil : $0 })
            }
            let options = Array(all.prefix(Self.maxOptions))
            guard !options.isEmpty else { return nil }
            self.id = id
            self.options = options
            replyOption = (alondra["reply_option_id"] as? String).flatMap { reply in
                all.first { $0.id == reply && $0.textInput != nil }
            }
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
            public let id: String
        }

        /// IDs that can appear in a category or action identifier unchanged.
        public static func isSafe(_ id: String) -> Bool {
            (1...128).contains(id.utf8.count)
                && id.utf8.allSatisfy { (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95 }
        }

        public static func questionID(_ id: String, binding: Binding) -> String {
            questionPrefix + binding.value + "_" + id
        }

        public static func questionID(from identifier: String) -> QuestionIdentity? {
            guard identifier.hasPrefix(questionPrefix) else { return nil }
            let suffix = identifier.dropFirst(questionPrefix.count)
            guard suffix.count > 44,
                  let binding = Binding(String(suffix.prefix(43))),
                  suffix.dropFirst(43).first == "_" else { return nil }
            let id = String(suffix.dropFirst(44))
            guard isSafe(id) else { return nil }
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

        public static func option(_ id: String) -> String { optionPrefix + id }
    }
}
