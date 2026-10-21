import AlondraContract
import SwiftUI

/// What the app around a card does with its MCP sign-in buttons: the sheet lives at the root, so
/// it outlives the card that opened it.
public struct McpLinks: Sendable {
    let signIn: @MainActor @Sendable (_ card: String, _ server: String) -> Void
    let reconnect: @MainActor @Sendable (_ card: String) -> Void

    public init(signIn: @escaping @MainActor @Sendable (String, String) -> Void,
                reconnect: @escaping @MainActor @Sendable (String) -> Void) {
        self.signIn = signIn
        self.reconnect = reconnect
    }

    public static let none = McpLinks(signIn: { _, _ in }, reconnect: { _ in })

    /// Runs a card's local link when it is an MCP one; false for any other.
    @MainActor @discardableResult
    public func callAsFunction(_ link: AlondraContract.Link?) -> Bool {
        switch link {
        case let .local(.mcpSignIn(card, server))?: signIn(card, server)
        case let .local(.mcpReconnect(card))?: reconnect(card)
        default: return false
        }
        return true
    }
}

public extension EnvironmentValues {
    @Entry var mcpLinks = McpLinks.none
}
