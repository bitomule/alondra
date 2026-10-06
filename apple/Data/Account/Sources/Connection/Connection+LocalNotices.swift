import AlondraNotifications
import AlondraTransport
import Foundation

extension Connection {
    /// The notices the daemon on this Mac let through, for the Mac app's system notifications.
    /// Only the local daemon answers it: a remote Mac's phone pushes are its own.
    public func localNotices() async throws -> LocalNotices {
        let preferred = Locale.preferredLanguages.first?.split(separator: "-").first.map(String.init) ?? "en"
        return try await api.send(.json(.get, path: ["notices"], query: [("lang", preferred == "es" ? "es" : "en")]))
    }
}
