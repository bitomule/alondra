import Foundation

public enum ServiceHosts {
    public static let relayHost = "api.getalondra.com"
    private static let previousRelayHost = "alondra-api.davidcollado.dev"

    /// Saved pairings made against the previous relay host keep working on the current one.
    public static func current(_ url: URL?) -> URL? {
        guard let url, url.host()?.lowercased() == previousRelayHost,
              var parts = URLComponents(url: url, resolvingAgainstBaseURL: false) else { return url }
        parts.host = relayHost
        return parts.url ?? url
    }
}
