import Foundation

/// The app's own `alondra://` links, which the Live Activity opens when it is tapped.
public enum AlondraLink: String, CaseIterable, Sendable {
    /// Waiting for You.
    case inbox
    /// Tasks, where a task that waits or works shows.
    case tasks

    public static let scheme = "alondra"

    public init?(url: URL) {
        guard url.scheme?.lowercased() == Self.scheme, let host = url.host()?.lowercased(),
              url.path().isEmpty || url.path() == "/", let link = Self(rawValue: host) else { return nil }
        self = link
    }

    public var url: URL { URL(string: "\(Self.scheme)://\(rawValue)")! }
}
