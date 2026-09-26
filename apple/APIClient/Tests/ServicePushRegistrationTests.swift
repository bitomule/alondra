import AlondraAPIClient
import Foundation
import Testing

@Suite struct ServicePushRegistrationTests {
    @Test func apnsTokenStaysOnControlPlaneAndMacOnlyReceivesBinding() async throws {
        let transport = PushTransport()
        let api = AlondraAPIClient(serviceURL: URL(string: "https://example.test")!, deviceID: "client-1", operations: transport)
        let binding = String(repeating: "A", count: 43)
        let registrationID = UUID()
        try await api.registerServicePush(token: "abcd", environment: "sandbox", binding: binding, preferencesRevision: 1, registrationID: registrationID)
        let events = await transport.events
        #expect(events == ["registration", "binding"])
        let request = try #require(await transport.request)
        #expect(request.path == "/v1/service/push-binding")
        let body = try #require(request.body)
        let object = try #require(JSONSerialization.jsonObject(with: body) as? [String: Any])
        #expect(object["push_binding"] as? String == binding)
        #expect(object["quiet_start"] is NSNull)
        #expect(object["quiet_end"] is NSNull)
        #expect(object["preferences_revision"] as? Int == 1)
        #expect(object.count == 4)
        #expect(request.operationID == registrationID.uuidString)
    }
    @Test func quietHoursTravelOnlyInEncryptedMacBindingAndCanBeDisabled() async throws {
        let transport = PushTransport()
        let api = AlondraAPIClient(serviceURL: URL(string: "https://example.test")!, deviceID: "client-1", operations: transport)
        let binding = String(repeating: "A", count: 43)
        try await api.registerServicePush(token: "abcd", environment: "sandbox", binding: binding, preferencesRevision: 1,
                                          quietStart: "23:00", quietEnd: "08:00")
        let enabled = try #require(await transport.request)
        let enabledData = try #require(enabled.body)
        let object = try #require(JSONSerialization.jsonObject(with: enabledData) as? [String: Any])
        #expect(object["quiet_start"] as? String == "23:00")
        #expect(object["quiet_end"] as? String == "08:00")
        try await api.registerServicePush(token: "abcd", environment: "sandbox", binding: binding, preferencesRevision: 2)
        let disabled = try #require(await transport.request)
        #expect(disabled.operationID != enabled.operationID)
        let disabledData = try #require(disabled.body)
        let disabledBody = try #require(JSONSerialization.jsonObject(with: disabledData) as? [String: Any])
        #expect(disabledBody["quiet_start"] is NSNull)
        #expect(disabledBody["quiet_end"] is NSNull)
        try await api.registerServicePush(token: "abcd", environment: "sandbox", binding: binding, preferencesRevision: 3,
                                          quietStart: "23:00", quietEnd: "08:00")
        let reenabled = try #require(await transport.request)
        #expect(reenabled.operationID != enabled.operationID)
    }

    @Test func authenticatedStalePreferenceResponseExposesRevisionForRecovery() async throws {
        let transport = PushTransport(response: .completed(status: 409,
            body: Data(#"{"kind":"stale_push_preferences","current_revision":77}"#.utf8)))
        let api = AlondraAPIClient(serviceURL: URL(string: "https://example.test")!, deviceID: "client-1", operations: transport)
        do {
            try await api.registerServicePush(token: "abcd", environment: "sandbox",
                binding: String(repeating: "A", count: 43), preferencesRevision: 1)
            Issue.record("Stale configuration was accepted")
        } catch ServicePushPreferencesError.stale(let revision) {
            #expect(revision == 77)
        }
    }

}

private actor PushTransport: ServiceOperationTransport, ServicePushTransport {
    let response: ServiceResponse
    init(response: ServiceResponse = .completed(status: 200, body: Data("{}".utf8))) { self.response = response }
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
        return response
    }
    func waitForChange(afterSnapshotTick: UInt64, seconds: Int) async throws {}
}
