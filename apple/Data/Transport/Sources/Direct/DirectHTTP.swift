import Foundation

/// Plain HTTP to a Mac's direct port. Everything that matters travels sealed, so `http` is
/// accepted here and nowhere else; the body limits match the daemon's.
public struct DirectHTTP: Sendable {
    static let maxRequest = 256 * 1024
    static let maxResponse = 1024 * 1024

    private let transport: any HTTPTransport

    public init(transport: any HTTPTransport) { self.transport = transport }

    public func hello(at endpoint: DirectEndpoint, timeout: Duration = .seconds(3)) async throws -> DirectMacIdentity {
        let reply = try await get(endpoint, path: "/direct/hello", timeout: timeout)
        guard reply.status == 200,
              let identity = try? JSONDecoder().decode(DirectMacIdentity.self, from: reply.body),
              identity.isValid else { throw RelayWireError.invalidResponse }
        return identity
    }

    public func get(_ endpoint: DirectEndpoint, path: String, timeout: Duration) async throws -> HTTPReply {
        try await send(endpoint, method: "GET", path: path, body: nil, timeout: timeout)
    }

    public func post(_ endpoint: DirectEndpoint, path: String, body: Data, timeout: Duration) async throws -> HTTPReply {
        guard body.count <= Self.maxRequest else { throw RelayWireError.invalidField }
        return try await send(endpoint, method: "POST", path: path, body: body, timeout: timeout)
    }

    private func send(_ endpoint: DirectEndpoint, method: String, path: String, body: Data?,
                      timeout: Duration) async throws -> HTTPReply {
        guard endpoint.isValid, path.hasPrefix("/direct/"), let url = endpoint.url(path) else {
            throw RelayWireError.invalidEndpoint
        }
        let remaining = try MutationScope.remaining(cap: timeout)
        guard remaining > .zero else { throw URLError(.timedOut) }
        var request = URLRequest(url: url, timeoutInterval: remaining.requestTimeInterval)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let body {
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        try MutationScope.check()
        if method == "POST" { try MutationScope.possibleDispatch() }
        let reply = try await transport.send(request)
        guard reply.body.count <= Self.maxResponse else { throw RelayWireError.invalidResponse }
        return reply
    }
}
