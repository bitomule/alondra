import AlondraTransport
import Foundation
import Synchronization
import Testing

@Suite struct ClientPresenceTests {
    @Test func inputReportsToEveryMacAtMostEveryThirtySeconds() async throws {
        let time = Mutex<Duration>(.zero)
        let presence = ClientPresence(now: { time.withLock { $0 } })
        let first = PresenceOperations(), second = PresenceOperations()
        let clients = ["first": client(first), "second": client(second)]
        await presence.activity(active: false, clients: clients)
        #expect(await first.requests.isEmpty)
        for seconds in stride(from: 0, to: 30, by: 3) {
            time.withLock { $0 = .seconds(seconds) }
            await presence.activity(active: true, clients: clients)
        }
        #expect(await first.requests.count == 1)
        #expect(await second.requests.count == 1)
        time.withLock { $0 = .seconds(30) }
        await presence.activity(active: true, clients: clients)
        #expect(await first.requests.count == 2)
        #expect(await second.requests.count == 2)
        let request = try #require(await first.requests.first)
        #expect(request.method == "POST")
        #expect(request.path == "/activity")
        let body = try #require(request.body)
        #expect(try JSONDecoder().decode([String: String].self, from: body) == ["client": "mac"])
        time.withLock { $0 = .seconds(150) }
        await presence.activity(active: false, clients: clients)
        #expect(await first.requests.count == 2)
    }

    @Test func aNewConnectionDoesNotWaitForTheOtherMacsThrottle() async {
        let presence = ClientPresence(now: { .zero })
        let first = PresenceOperations(), second = PresenceOperations()
        await presence.activity(active: true, clients: ["first": client(first)])
        await presence.activity(active: true, clients: ["first": client(first), "second": client(second)])
        #expect(await first.requests.count == 1)
        #expect(await second.requests.count == 1)
    }

    private func client(_ operations: PresenceOperations) -> AlondraAPIClient {
        AlondraAPIClient(serviceURL: URL(string: "https://example.test")!, deviceID: "mac", operations: operations)
    }
}

private actor PresenceOperations: OperationAPI {
    var requests: [APIRequest] = []
    func perform(_ request: APIRequest) async throws -> APIResponse {
        requests.append(request)
        return .completed(status: 200, body: Data("{}".utf8))
    }
}
