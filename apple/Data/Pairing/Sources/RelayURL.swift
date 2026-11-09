import Foundation

public enum RelayURL {
    /// One relay has one spelling: lowercase scheme and host, no default HTTPS port, no trailing slash.
    public static func canonical(_ url: URL?) -> URL? {
        guard let url, var parts = URLComponents(url: url, resolvingAgainstBaseURL: false) else { return url }
        parts.scheme = parts.scheme?.lowercased()
        parts.host = parts.host?.lowercased()
        if parts.scheme == "https", parts.port == 443 { parts.port = nil }
        while parts.path.hasSuffix("/") { parts.path.removeLast() }
        return parts.url ?? url
    }
}
