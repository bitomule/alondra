import Foundation

/// The lock-screen text of an authenticated push, and how it is delivered. A nil body means the
/// Mac had nothing to say about the card, and the extension shows its own localized generic line.
/// The sealed payload carries its own `interruption-level` and `thread-id`, so the extension
/// applies them: the sealed envelope's outer alert cannot know what the card is.
public struct NotificationAlert: Equatable, Sendable {
    public enum Level: String, Equatable, Sendable {
        case passive, active
        case timeSensitive = "time-sensitive"
    }

    public let title: String
    public let body: String?
    /// How loudly the card interrupts; nil when the payload names none.
    public let level: Level?
    /// The notifications of one node group together under this thread.
    public let thread: String?

    public init(aps: [String: Any]) {
        let alert = aps["alert"] as? [String: Any]
        let title = (alert?["title"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let body = (alert?["body"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        self.title = title.isEmpty ? "Alondra" : title
        self.body = body.isEmpty ? nil : body
        level = (aps["interruption-level"] as? String).flatMap(Level.init(rawValue:))
        thread = (aps["thread-id"] as? String).flatMap { $0.isEmpty ? nil : $0 }
    }
}
