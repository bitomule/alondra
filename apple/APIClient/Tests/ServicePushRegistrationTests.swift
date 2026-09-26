import AlondraAPIClient
import Foundation
import Testing

@Suite struct ServicePushRegistrationTests {
    @Test func apnsTokenStaysOnControlPlaneAndMacOnlyReceivesBinding() async throws {
        let transport = PushTransport()
        let api = AlondraAPIClient(serviceURL: URL(string: "https://example.test")!, deviceID: "client-1", operations: transport)
        let binding = String(repeating: "A", count: 43)
        try await api.registerServicePush(token: "abcd", environment: "sandbox", binding: binding)
        let events = await transport.events
        #expect(events == ["registration", "binding"])
        let request = try #require(await transport.request)
        #expect(request.path == "/v1/service/push-binding")
        #expect(request.operationID == "push-binding-" + binding)
        let body = try #require(request.body)
        let object = try #require(JSONSerialization.jsonObject(with: body) as? [String: String])
        #expect(object == ["push_binding": binding])
    }
}

private actor PushTransport: ServiceOperationTransport, ServicePushTransport {
    var events: [String] = []
    var request: ServiceRequest?
    func registerServicePush(token: String, environment: String, binding: String) async throws {
        #expect(token == "abcd")
        #expect(environment == "sandbox")
        events.append("registration")
    }
    func perform(_ request: ServiceRequest) async throws -> ServiceResponse {
        events.append("binding")
        self.request = request
        return .completed(status: 200, body: Data("{}".utf8))
    }
    func waitForChange(afterSnapshotTick: UInt64, seconds: Int) async throws {}
}
