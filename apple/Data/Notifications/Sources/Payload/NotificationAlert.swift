import Foundation

/// The lock-screen text of an authenticated push. A nil body means the Mac had nothing to
/// say about the card, and the extension shows its own localized generic line.
public struct NotificationAlert: Equatable, Sendable {
    public let title: String
    public let body: String?

    public init(aps: [String: Any]) {
        let alert = aps["alert"] as? [String: Any]
        let title = (alert?["title"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let body = (alert?["body"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        self.title = title.isEmpty ? "Alondra" : title
        self.body = body.isEmpty ? nil : body
    }
}
