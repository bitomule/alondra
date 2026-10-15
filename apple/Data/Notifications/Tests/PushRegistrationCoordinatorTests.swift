import AlondraNotifications
import AlondraTransport
import Foundation
import Testing
@testable import AlondraNotifications

private actor RegistrationRecorder {
    struct Call: Sendable { let mac: String; let token: String; let revision: UInt64; let quiet: String? }
    var calls: [Call] = []
    var shouldPause = false
    var entered: CheckedContinuation<Void, Never>?
    var release: CheckedContinuation<Void, Never>?
    var failMac: String?
    var queued: [String: [any Error]] = [:]
    func register(mac: String, token: String, revision: UInt64, quiet: String?) async throws {
        calls.append(Call(mac: mac, token: token, revision: revision, quiet: quiet))
        if shouldPause {
            shouldPause = false
            entered?.resume(); entered = nil
            await withCheckedContinuation { release = $0 }
        }
        if failMac == mac { throw Failure.offline }
        if let error = queued[mac]?.first { queued[mac]?.removeFirst(); throw error }
    }
    func pauseNext() { shouldPause = true }
    func waitUntilPaused() async { if release == nil { await withCheckedContinuation { entered = $0 } } }
    func resume() { release?.resume(); release = nil }
    func fail(_ mac: String) { failMac = mac }
    func fail(_ mac: String, with errors: [any Error]) { queued[mac] = errors }
    enum Failure: Error { case offline }
}

private func registrationClient(_ n: UInt8, recorder: RegistrationRecorder) -> NotificationClient {
    let binding = NotificationPayload.Binding(Data(repeating: n, count: 32).base64EncodedString()
        .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: ""))!
    return NotificationClient(identity: "mac-\(n)", binding: binding, register: { token, _, start, _, revision in
        try await recorder.register(mac: "mac-\(n)", token: token, revision: revision, quiet: start)
    }, submit: { _ in .uncertain })
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

@Test @MainActor func anExpiredRegistrationIsSentAgainWithTheSameToken() async throws {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: directory) }
    let coordinator = PushRegistrationCoordinator(preferences: PushPreferencesLedger(file: directory.appendingPathComponent("ledger.json")))
    let recorder = RegistrationRecorder()
    let a = registrationClient(1, recorder: recorder)
    try coordinator.update(clients: [a], token: "token-a", environment: "sandbox", quietStart: nil, quietEnd: nil)
    await coordinator.waitForPendingRegistration()
    coordinator.expireRegistrations()
    try coordinator.update(clients: [a], token: "token-a", environment: "sandbox", quietStart: nil, quietEnd: nil)
    await coordinator.waitForPendingRegistration()
    let calls = await recorder.calls
    #expect(calls.map(\.token) == ["token-a", "token-a"])
    #expect(calls.map(\.revision) == [1, 1])
    #expect(coordinator.state == .registered)
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

@Test @MainActor func failedRegistrationShowsItsOwnMessageNotTheErrorText() async throws {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: directory) }
    let coordinator = PushRegistrationCoordinator(preferences: PushPreferencesLedger(file: directory.appendingPathComponent("ledger.json")))
    let recorder = RegistrationRecorder()
    await recorder.fail("mac-1")
    try coordinator.update(clients: [registrationClient(1, recorder: recorder)],
        token: "token", environment: "sandbox", quietStart: nil, quietEnd: nil)
    await coordinator.waitForPendingRegistration()
    #expect(coordinator.state == .failed("Alerts couldn’t be set up on this device. Alondra tries again when you reopen the app."))
}

@Test(arguments: [
    (URLError(.timedOut) as any Error, PushRegistrationCoordinator.Problem.unreachable),
    (URLError(.notConnectedToInternet) as any Error, .unreachable),
    (RelayWireError.outcomeUnknown as any Error, .unreachable),
    (RelaySessionError.outcomeUnknown as any Error, .unreachable),
    (OperationError.outcomeUnknown as any Error, .unreachable),
    (APIError.unavailable as any Error, .unreachable),
    (APIError.unexpected(status: 500) as any Error, .unreachable),
    (RelayWireError.unauthorized as any Error, .relay),
    (RelayWireError.server(502) as any Error, .relay),
    (RelayWireError.server(503) as any Error, .relay),
    (RelayWireError.admissionFull as any Error, .relay),
    (APIError.unauthorized as any Error, .rejected),
    (APIError.forbidden(kind: nil, detail: nil, reasons: []) as any Error, .rejected),
    (APIError.server(status: 422, kind: nil, detail: "bad binding", reasons: []) as any Error, .rejected),
    (PushPreferencesError.stale(currentRevision: 3) as any Error, .rejected),
    (CocoaError(.fileReadCorruptFile) as any Error, .local),
])
func everyRegistrationFailureIsClassifiedByWhoFailed(error: any Error, problem: PushRegistrationCoordinator.Problem) {
    #expect(PushRegistrationCoordinator.problem(error) == problem)
}

@Test func onlyARealRefusalFromTheMacSaysTheMacDidNotAccept() {
    #expect(PushRegistrationCoordinator.Problem.rejected.message
        == "Your Mac didn’t accept this device’s alerts. Alondra tries again when you reopen the app.")
    #expect(PushRegistrationCoordinator.Problem.unreachable.message == "Can’t reach your Mac. Retrying…")
    #expect(PushRegistrationCoordinator.Problem.relay.message == "Alondra’s relay isn’t responding. Retrying…")
    let messages = [PushRegistrationCoordinator.Problem.unreachable, .relay, .rejected, .local].map(\.message)
    #expect(Set(messages).count == messages.count)
}

@Test func retryWaitGrowsAndStopsAtFiveMinutes() {
    let waits = (0..<9).map { PushRegistrationCoordinator.retryDelay(attempt: $0) }
    #expect(waits == [5, 10, 20, 40, 80, 160, 300, 300, 300].map { Duration.seconds($0) })
}

private actor Pauses {
    var waits: [Duration] = []
    func record(_ wait: Duration) { waits.append(wait) }
}

@Test @MainActor func networkAndRelayFailuresRetryInTheBackgroundUntilTheRegistrationWorks() async throws {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: directory) }
    let pauses = Pauses()
    let coordinator = PushRegistrationCoordinator(preferences: PushPreferencesLedger(file: directory.appendingPathComponent("ledger.json")),
        pause: { await pauses.record($0) })
    var states: [PushRegistrationCoordinator.State] = []
    coordinator.onChange = { states.append($0) }
    let recorder = RegistrationRecorder()
    await recorder.fail("mac-1", with: [URLError(.timedOut), RelayWireError.server(503), RelayWireError.unauthorized])
    try coordinator.update(clients: [registrationClient(1, recorder: recorder), registrationClient(2, recorder: recorder)],
        token: "token", environment: "sandbox", quietStart: nil, quietEnd: nil)
    await coordinator.waitForPendingRegistration()
    #expect(coordinator.state == .registered)
    #expect(await pauses.waits == [.seconds(5), .seconds(10), .seconds(20)])
    #expect(await recorder.calls.map(\.mac) == ["mac-1", "mac-2", "mac-1", "mac-1", "mac-1"])
    #expect(states == [
        .sending,
        .failed("Can’t reach your Mac. Retrying…"),
        .failed("Alondra’s relay isn’t responding. Retrying…"),
        .registered,
    ])
}

@Test @MainActor func aRefusalFromTheMacIsNotRetried() async throws {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: directory) }
    let pauses = Pauses()
    let coordinator = PushRegistrationCoordinator(preferences: PushPreferencesLedger(file: directory.appendingPathComponent("ledger.json")),
        pause: { await pauses.record($0) })
    let recorder = RegistrationRecorder()
    await recorder.fail("mac-1", with: [APIError.forbidden(kind: nil, detail: nil, reasons: [])])
    try coordinator.update(clients: [registrationClient(1, recorder: recorder)],
        token: "token", environment: "sandbox", quietStart: nil, quietEnd: nil)
    await coordinator.waitForPendingRegistration()
    #expect(coordinator.state == .failed("Your Mac didn’t accept this device’s alerts. Alondra tries again when you reopen the app."))
    #expect(await pauses.waits.isEmpty)
    #expect(await recorder.calls.count == 1)
}

@Test @MainActor func signingOutStopsAPendingRetry() async throws {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: directory) }
    let coordinator = PushRegistrationCoordinator(preferences: PushPreferencesLedger(file: directory.appendingPathComponent("ledger.json")))
    let recorder = RegistrationRecorder()
    await recorder.fail("mac-1", with: [URLError(.timedOut)])
    try coordinator.update(clients: [registrationClient(1, recorder: recorder)],
        token: "token", environment: "sandbox", quietStart: nil, quietEnd: nil)
    while coordinator.state != .failed("Can’t reach your Mac. Retrying…") { await Task.yield() }
    coordinator.clear()
    await coordinator.waitForPendingRegistration()
    #expect(coordinator.state == .idle)
    #expect(await recorder.calls.count == 1)
}

@Test @MainActor func duplicateMacsAreRejectedWithAReadableReason() throws {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: directory) }
    let coordinator = PushRegistrationCoordinator(preferences: PushPreferencesLedger(file: directory.appendingPathComponent("ledger.json")))
    let recorder = RegistrationRecorder()
    #expect(throws: PushRegistrationCoordinator.Failure.duplicateIdentity) {
        try coordinator.update(clients: [registrationClient(1, recorder: recorder), registrationClient(1, recorder: recorder)],
            token: "token", environment: "sandbox", quietStart: nil, quietEnd: nil)
    }
    #expect(PushRegistrationCoordinator.Failure.duplicateIdentity.errorDescription
        == "Two linked Macs look like the same Mac. Remove one in Settings and link it again.")
}
