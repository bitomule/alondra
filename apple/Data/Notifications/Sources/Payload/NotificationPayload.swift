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

    /// A views push names the card and carries the exact opaque actions shown by the server.
    public struct Item: Hashable, Sendable {
        public enum Kind: String, Hashable, Sendable {
            case question, actionRequest = "action_request", information, delivery, pullRequest = "pull_request"
        }

        public let kind: Kind
        public let card: String
        public let epoch: String
        public let actions: [PushAction]

        public var itemID: String { card }
        public var inboxID: String { "interaction:\(card)" }
        public var interactionID: String? { card }

        public init?(userInfo: [AnyHashable: Any]) {
            guard let alondra = userInfo["alondra"] as? [String: Any],
                  (alondra["protocol"] as? NSNumber)?.intValue == 1,
                  let kind = (alondra["kind"] as? String).flatMap(Kind.init(rawValue:)),
                  let card = alondra["card"] as? String, Category.isSafe(card),
                  let epoch = alondra["data_epoch"] as? String, !epoch.isEmpty,
                  let raw = alondra["actions"] as? [[String: Any]]
            else { return nil }
            self.kind = kind
            self.card = card
            self.epoch = epoch
            self.actions = Array(raw.prefix(4)).compactMap { entry in
                guard let id = entry["id"] as? String, !id.isEmpty, id.utf8.count <= 512,
                      let label = (entry["label"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines),
                      !label.isEmpty else { return nil }
                let option = (entry["option"] as? String).flatMap { $0.isEmpty ? nil : $0 }
                let text = (entry["text"] as? Bool) ?? false
                let placeholder = (entry["placeholder"] as? String).flatMap { $0.isEmpty ? nil : $0 }
                let opens = (entry["open"] as? Bool) ?? false
                return PushAction(id: id, option: option, label: label, text: text, placeholder: placeholder, opens: opens)
            }
        }

        /// The button opens the card in the app instead of acting from the notification.
        public func opens(_ action: String) -> Bool {
            guard let index = Action.index(action), actions.indices.contains(index) else { return false }
            return actions[index].opens
        }
    }

    public struct PushAction: Hashable, Sendable {
        public let id: String
        public let option: String?
        public let label: String
        public let text: Bool
        public let placeholder: String?
        public let opens: Bool
    }

    public enum Category {
        public static let question = "ALONDRA_PUSH"
        public static let permission = "ALONDRA_PUSH"
        public static let plan = "ALONDRA_PUSH"
        public static let read = "ALONDRA_PUSH"
        public static let questionPrefix = "ALONDRA_"

        public struct QuestionIdentity: Equatable, Sendable {
            public let binding: Binding
            public let id: String
        }

        /// IDs that can appear in a category or action identifier unchanged.
        public static func isSafe(_ id: String) -> Bool {
            guard (1...256).contains(id.utf8.count) else { return false }
            if id.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95 }) { return true }
            guard id.hasPrefix("github.com/"), let hash = id.firstIndex(of: "#") else { return false }
            let repo = id[id.index(id.startIndex, offsetBy: "github.com/".count)..<hash]
            let parts = repo.split(separator: "/", omittingEmptySubsequences: false)
            guard parts.count == 2, parts.allSatisfy({ part in
                !part.isEmpty && part != "." && part != ".." && part.utf8.allSatisfy {
                    (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95 || $0 == 46
                }
            }) else { return false }
            let number = id[id.index(after: hash)...]
            return !number.isEmpty && number.utf8.allSatisfy({ (48...57).contains($0) })
                && (UInt64(number).map { $0 > 0 } ?? false)
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
        public static func indexed(_ index: Int) -> String { "A\(index)" }
        public static func index(_ identifier: String) -> Int? {
            let bytes = Array(identifier.utf8)
            guard bytes.count == 2, bytes[0] == 65, (48...51).contains(bytes[1]) else { return nil }
            return Int(bytes[1] - 48)
        }
    }
}
