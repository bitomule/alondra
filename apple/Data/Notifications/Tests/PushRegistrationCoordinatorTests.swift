import AlondraNotificationPayload
import Foundation
import Testing
@testable import AlondraNotificationActions

private actor RegistrationRecorder {
    struct Call: Sendable { let mac: String; let token: String; let revision: UInt64; let quiet: String? }
    var calls: [Call] = []
    var shouldPause = false
    var entered: CheckedContinuation<Void, Never>?
    var release: CheckedContinuation<Void, Never>?
    var failMac: String?
    func register(mac: String, token: String, revision: UInt64, quiet: String?) async throws {
        calls.append(Call(mac: mac, token: token, revision: revision, quiet: quiet))
        if shouldPause {
            shouldPause = false
            entered?.resume(); entered = nil
            await withCheckedContinuation { release = $0 }
        }
        if failMac == mac { throw Failure.offline }
    }
    func pauseNext() { shouldPause = true }
    func waitUntilPaused() async { if release == nil { await withCheckedContinuation { entered = $0 } } }
    func resume() { release?.resume(); release = nil }
    func fail(_ mac: String) { failMac = mac }
    enum Failure: Error { case offline }
}

private func registrationClient(_ n: UInt8, recorder: RegistrationRecorder) -> ServiceNotificationClient {
    let binding = NotificationPayload.Binding(Data(repeating: n, count: 32).base64EncodedString()
        .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: ""))!
    return ServiceNotificationClient(identity: "mac-\(n)", binding: binding, register: { token, _, start, _, revision in
        try await recorder.register(mac: "mac-\(n)", token: token, revision: revision, quiet: start)
    }, submit: { _, _ in .sent(.obsolete) })
}

@Test @MainActor func oneDeviceTokenRegistersEveryMacAndSelectionDoesNotRotatePreferences() async throws {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: directory) }
    let coordinator = PushRegistrationCoordinator(preferences: PushPreferencesLedger(file: directory.appendingPathComponent("ledger.json")))
    let recorder = RegistrationRecorder()
    let a = registrationClient(1, recorder: recorder), b = registrationClient(2, recorder: recorder)
    try coordinator.update(clients: [a, b], token: "token-a", environment: "sandbox", quietStart: nil, quietEnd: nil)
    await coordinator.waitForPendingRegistration()
    #expect(coordinator.state == .registered)
    #expect(await recorder.calls.map(\.mac) == ["mac-1", "mac-2"])
    try coordinator.update(clients: [b, a], token: "token-a", environment: "sandbox", quietStart: nil, quietEnd: nil)
    await coordinator.waitForPendingRegistration()
    #expect(await recorder.calls.count == 2)
    try coordinator.update(clients: [a, b], token: "token-b", environment: "sandbox", quietStart: "23:00", quietEnd: "08:00")
    await coordinator.waitForPendingRegistration()
    let calls = await recorder.calls
    #expect(calls.count == 4)
    #expect(calls.suffix(2).allSatisfy { $0.token == "token-b" && $0.revision == 2 && $0.quiet == "23:00" })
    #expect(throws: (any Error).self) {
        try coordinator.update(clients: [a, a], token: "token-b", environment: "sandbox", quietStart: nil, quietEnd: nil)
    }
}

@Test @MainActor func logoutAndReplacementRejectLateRegistrationCompletion() async throws {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: directory) }
    let coordinator = PushRegistrationCoordinator(preferences: PushPreferencesLedger(file: directory.appendingPathComponent("ledger.json")))
    let recorder = RegistrationRecorder()
    let a = registrationClient(1, recorder: recorder), b = registrationClient(2, recorder: recorder)
    await recorder.pauseNext()
    try coordinator.update(clients: [a], token: "old", environment: "sandbox", quietStart: nil, quietEnd: nil)
    await recorder.waitUntilPaused()
    coordinator.clear()
    #expect(coordinator.state == .idle)
    try coordinator.update(clients: [b], token: "new", environment: "sandbox", quietStart: nil, quietEnd: nil)
    await recorder.resume()
    await coordinator.waitForPendingRegistration()
    #expect(coordinator.state == .registered)
    let calls = await recorder.calls
    #expect(calls.map(\.mac) == ["mac-1", "mac-2"])
    #expect(calls.map(\.token) == ["old", "new"])
    // A was removed; re-adding it must perform a registration, not accept its late success.
    try coordinator.update(clients: [a, b], token: "new", environment: "sandbox", quietStart: nil, quietEnd: nil)
    await coordinator.waitForPendingRegistration()
    #expect(await recorder.calls.count == 3)
    #expect(await recorder.calls.last?.mac == "mac-1")
}

@Test @MainActor func offlineMacDoesNotPreventRegisteringOtherApprovedMac() async throws {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: directory) }
    let coordinator = PushRegistrationCoordinator(preferences: PushPreferencesLedger(file: directory.appendingPathComponent("ledger.json")))
    let recorder = RegistrationRecorder()
    await recorder.fail("mac-1")
    try coordinator.update(clients: [registrationClient(1, recorder: recorder), registrationClient(2, recorder: recorder)],
        token: "token", environment: "sandbox", quietStart: nil, quietEnd: nil)
    await coordinator.waitForPendingRegistration()
    #expect(await recorder.calls.map(\.mac) == ["mac-1", "mac-2"])
    guard case .failed = coordinator.state else { Issue.record("offline registration must remain visible"); return }
}
