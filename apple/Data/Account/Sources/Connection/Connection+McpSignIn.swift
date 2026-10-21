import AlondraFeatures
import AlondraTransport
import Foundation

public extension Connection {
    /// Connect, Reconnect and the browser's return for the cards of tasks that wait for an MCP
    /// server. None of these requests is an action: they go sealed to the Mac like every request,
    /// and nothing of them is journaled on this device.
    nonisolated func mcpSignInClient(browser: McpSignInBrowser) -> McpSignInClient {
        let api = api
        @Sendable func post(_ card: String, _ step: String, _ body: [String: String] = [:]) async throws -> Data {
            let bytes = try JSONSerialization.data(withJSONObject: body)
            return try await api.send(Endpoint<Data>.data(.post, path: ["mcp-sign-ins", card, step], body: bytes,
                                                         contentType: "application/json", requiresJSONObjectResponse: true),
                                      operationID: "mcp-" + UUID().uuidString)
        }
        return McpSignInClient(
            connect: { card in
                guard let page = McpSignInPage(daemonReply: try await post(card, "connect")) else { throw OperationError.outcomeUnknown }
                return page
            },
            giveBack: { card, address in _ = try await post(card, "return", ["address": address.absoluteString]) },
            reconnect: { card in _ = try await post(card, "reconnect") },
            open: { url, port in browser.open(url, port: port) }
        )
    }
}
