import ComposableArchitecture
import Foundation

/// What the Mac answered to Connect on the card of the tasks that wait for an MCP server.
public enum McpSignInPage: Equatable, Sendable {
    /// Sign in at `url`. With a `port` the browser then comes back to
    /// `http://localhost:<port>/callback`; without one it is a claude.ai connector, connected on
    /// claude.ai and then reconnected.
    case browser(url: URL, port: UInt16?)
    /// Claude's stored sign-in already works.
    case connected
}

/// Where the browser goes once the user signed in.
public enum McpReturnRoute: Equatable, Sendable {
    /// This device listens on its own localhost and brings the return to the Mac by itself.
    case listening
    /// This is the daemon's Mac: Claude's own listener holds the port and takes the return.
    case claudeOnThisMac
    /// Nothing here can take it: the user pastes the address of the page that did not load.
    case paste
}

/// What happens to the sign-in page once it is open on this device.
public enum McpSignInEvent: Equatable, Sendable {
    case opened(McpReturnRoute)
    /// Where the browser came back to: it carries a one-time code, kept only in memory.
    case returned(URL)
    /// The page closed without coming back here.
    case closed
}

@DependencyClient
public struct McpSignInClient: Sendable {
    public var connect: @Sendable (_ card: String) async throws -> McpSignInPage
    /// Hands where the browser came back to to the Mac, which gives it to Claude in memory.
    public var giveBack: @Sendable (_ card: String, _ address: URL) async throws -> Void
    public var reconnect: @Sendable (_ card: String) async throws -> Void
    /// Opens `url` and, with a `port`, listens on this device's own localhost for the return
    /// until it comes or the page closes.
    public var open: @Sendable (_ url: URL, _ port: UInt16?) -> AsyncStream<McpSignInEvent> = { _, _ in AsyncStream { $0.finish() } }
}

extension McpSignInClient: TestDependencyKey {
    public static let testValue = McpSignInClient()
}

public extension DependencyValues {
    var mcpSignIn: McpSignInClient {
        get { self[McpSignInClient.self] }
        set { self[McpSignInClient.self] = newValue }
    }
}

public extension McpSignInPage {
    /// The daemon's answer to `POST /mcp-sign-ins/{card}/connect`; nil for anything else, such as
    /// a page that is neither https nor this device's localhost.
    init?(daemonReply body: Data) {
        guard let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any] else { return nil }
        if object["connected"] as? Bool == true {
            self = .connected
            return
        }
        guard let raw = object["url"] as? String, let url = URL(string: raw),
              let scheme = url.scheme?.lowercased(), url.host != nil,
              scheme == "https" || (scheme == "http" && ["localhost", "127.0.0.1"].contains(url.host?.lowercased())) else { return nil }
        let port = (object["port"] as? NSNumber).map(\.intValue).flatMap { (1...65535).contains($0) ? UInt16($0) : nil }
        if object["returns_to_claude"] as? Bool == true, port == nil { return nil }
        self = .browser(url: url, port: object["returns_to_claude"] as? Bool == true ? port : nil)
    }
}

/// Where the browser came back to, as pasted by the user: the whole `http://localhost:<port>/callback?…`.
public enum McpReturnAddress {
    public static func parse(_ text: String, port: UInt16?) -> URL? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let components = URLComponents(string: trimmed), components.scheme?.lowercased() == "http",
              ["localhost", "127.0.0.1"].contains(components.host?.lowercased()), components.path == "/callback",
              port.map({ components.port == Int($0) }) ?? (components.port != nil),
              let items = components.queryItems, items.contains(where: { $0.name == "state" }),
              items.contains(where: { $0.name == "code" || $0.name == "error" }) else { return nil }
        return components.url
    }
}
