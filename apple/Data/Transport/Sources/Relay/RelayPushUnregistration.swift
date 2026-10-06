import Foundation

/// Unregistration outlives the account session, so it carries no bearer or device signature:
/// the random push binding is the only credential and the relay answers it idempotently.
public struct RelayPushUnregistration: Sendable {
    public enum Outcome: Equatable, Sendable { case done, retry, drop }

    private let transport: any HTTPTransport

    public init() { transport = URLSessionTransport() }
    public init(transport: any HTTPTransport) { self.transport = transport }

    public func unregister(binding: String, relay: URL) async -> Outcome {
        guard relay.scheme?.lowercased() == "https", relay.host != nil, relay.user == nil, relay.password == nil,
              relay.query == nil, relay.fragment == nil, relay.path.isEmpty || relay.path == "/",
              let url = URL(string: "/v1/push/unregister", relativeTo: relay)?.absoluteURL,
              let body = try? JSONSerialization.data(withJSONObject: ["push_binding": binding]) else { return .drop }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.timeoutInterval = 15
        request.httpBody = body
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        guard let reply = try? await transport.send(request) else { return .retry }
        return Self.outcome(status: reply.status)
    }

    static func outcome(status: Int) -> Outcome {
        switch status {
        case 200..<300: .done
        case 429: .retry
        case 400..<500: .drop
        default: .retry
        }
    }
}
