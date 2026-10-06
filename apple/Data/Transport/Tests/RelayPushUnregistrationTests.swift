import AlondraTransport
import Foundation
import Testing

private actor ScriptedHTTP: HTTPTransport {
    let reply: Result<Int, URLError>
    var requests: [URLRequest] = []
    init(_ reply: Result<Int, URLError>) { self.reply = reply }
    func send(_ request: URLRequest) async throws -> HTTPReply {
        requests.append(request)
        return HTTPReply(status: try reply.get(), body: Data())
    }
}

@Suite struct RelayPushUnregistrationTests {
    private let relay = URL(string: "https://relay.example")!
    private let binding = String(repeating: "A", count: 43)

    @Test func postsOnlyTheBindingWithoutSessionCredentials() async throws {
        let http = ScriptedHTTP(.success(204))
        let outcome = await RelayPushUnregistration(transport: http).unregister(binding: binding, relay: relay)
        #expect(outcome == .done)
        let request = try #require(await http.requests.first)
        #expect(request.httpMethod == "POST")
        #expect(request.url?.absoluteString == "https://relay.example/v1/push/unregister")
        #expect(request.value(forHTTPHeaderField: "Authorization") == nil)
        #expect(request.value(forHTTPHeaderField: "x-device-signature") == nil)
        let body = try #require(request.httpBody)
        #expect(try JSONSerialization.jsonObject(with: body) as? [String: String] == ["push_binding": binding])
    }

    @Test(arguments: [
        (Result<Int, URLError>.success(200), RelayPushUnregistration.Outcome.done),
        (.success(204), .done),
        (.success(400), .drop),
        (.success(413), .drop),
        (.success(404), .retry),
        (.success(429), .retry),
        (.success(500), .retry),
        (.success(503), .retry),
        (.failure(URLError(.notConnectedToInternet)), .retry),
    ])
    func outcomeFollowsTheRelayContract(reply: Result<Int, URLError>, expected: RelayPushUnregistration.Outcome) async {
        let outcome = await RelayPushUnregistration(transport: ScriptedHTTP(reply)).unregister(binding: binding, relay: relay)
        #expect(outcome == expected)
    }

    @Test func refusesARelayURLThatCouldRedirectTheBinding() async {
        let http = ScriptedHTTP(.success(204))
        let outcome = await RelayPushUnregistration(transport: http)
            .unregister(binding: binding, relay: URL(string: "http://relay.example")!)
        #expect(outcome == .drop)
        #expect(await http.requests.isEmpty)
    }
}
