import AlondraLiveActivity
@testable import AlondraNotifications
import AlondraTransport
import Foundation
import Testing

/// The real API facade and both coordinators run against a deterministic relay/Mac double.
@Test @MainActor func alertsAndActivityRecoverThroughTheRegistrationAPI() async throws {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: directory) }
    let transport = RecoveringRegistrationTransport()
    let api = AlondraAPIClient(serviceURL: URL(string: "https://example.test")!, deviceID: "test-phone", operations: transport)
    let binding = try #require(NotificationPayload.Binding(String(repeating: "A", count: 43)))
    let notifications = PushRegistrationCoordinator(preferences: PushPreferencesLedger(file: directory.appendingPathComponent("preferences.json")), pause: { _ in })
    let activity = ActivityRegistrar(phoneID: { "phone-install-0123456789" }, environment: "sandbox", pause: { _ in })
    var failures: [RegistrationFailure] = []
    notifications.onChange = { failures += $0.failures }
    activity.onChange = { failures += activity.relayState.failures + activity.keyState.failures }
    defer { activity.onChange = nil }
    let client = NotificationClient(identity: "test-mac", binding: binding, register: { token, environment, start, end, revision in
        try await api.registerServicePush(token: token, environment: environment, binding: binding.value,
            preferencesRevision: revision, quietStart: start, quietEnd: end)
    }, submit: { _ in .uncertain })
    try notifications.update(clients: [client], token: "test-token", environment: "sandbox", quietStart: "23:00", quietEnd: "08:00")
    activity.handle(.ready)
    activity.set([ActivityRelayPairing(id: "test-mac", register: { try await api.registerActivity($0) }, unregister: { _ in },
        publicKey: "test-key", giveKey: { try await api.giveActivityKey($0) })])
    await notifications.waitForPendingRegistration()
    await activity.settled()
    #expect(notifications.state == .registered)
    #expect(activity.relayState == .registered)
    #expect(activity.keyState == .registered)
    #expect(failures.contains(RegistrationFailure(operation: .mac, reason: .stalePreferences(77))))
    #expect(failures.contains(RegistrationFailure(operation: .relay, reason: .unavailable)))
    #expect(failures.contains(RegistrationFailure(operation: .activityKey, reason: .notEnrolled)))
    #expect(await transport.revisions == [1, 78])
    #expect(await transport.keyCalls == 2)
    #expect(await transport.activityCalls == 2)
}

private actor RecoveringRegistrationTransport: OperationAPI, PushRegistrationAPI {
    var revisions: [UInt64] = []
    var keyCalls = 0
    var activityCalls = 0
    func registerServicePush(token: String, environment: String, binding: String) async throws {}
    func registerActivity(_ registration: RelayActivityRegistration) async throws {
        activityCalls += 1
        if activityCalls == 1 { throw RelayWireError.server(503) }
    }
    func perform(_ request: APIRequest) async throws -> APIResponse {
        switch request.path {
        case "/device/push-binding":
            struct Body: Decodable { let preferences_revision: UInt64; let quiet_start: String; let quiet_end: String }
            let body = try JSONDecoder().decode(Body.self, from: #require(request.body))
            #expect(body.quiet_start == "23:00")
            #expect(body.quiet_end == "08:00")
            revisions.append(body.preferences_revision)
            if revisions.count == 1 {
                return .completed(status: 409, body: Data(#"{"kind":"stale_push_preferences","current_revision":77}"#.utf8))
            }
        case "/device/activity-key":
            keyCalls += 1
            if keyCalls == 1 { return .completed(status: 409, body: Data(#"{"kind":"device_not_enrolled"}"#.utf8)) }
        default: Issue.record("Unexpected request path")
        }
        return .completed(status: 204, body: Data())
    }
}
