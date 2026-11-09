import AlondraLiveActivity
import AlondraTransport
import Foundation
import Synchronization
import Testing

private actor ActivityAttempts {
    var relayCalls = 0
    var keyCalls = 0
    var waits: [Duration] = []
    func register() throws {
        relayCalls += 1
        if relayCalls == 1 { throw RelayWireError.server(503) }
        if relayCalls == 2 { throw RelayWireError.admissionFull }
    }
    func giveKey() throws {
        keyCalls += 1
        if keyCalls == 1 { throw APIError.conflict(kind: "device_not_enrolled", detail: nil, reasons: []) }
    }
    func pause(_ duration: Duration) { waits.append(duration) }
}

private actor ActivityGate {
    private var entered = false
    private var observer: CheckedContinuation<Void, Never>?
    private var release: CheckedContinuation<Void, Never>?
    func suspend() async {
        entered = true
        observer?.resume(); observer = nil
        await withCheckedContinuation { release = $0 }
    }
    func waitUntilEntered() async {
        if !entered { await withCheckedContinuation { observer = $0 } }
    }
    func resume() { release?.resume(); release = nil }
}

@MainActor @Suite struct ActivityRegistrationRecoveryTests {
    @Test func oneSuccessfulUnregistrationClearsFailuresFromOtherPairings() async {
        let removed = Mutex<[String]>([])
        let registrar = ActivityRegistrar(phoneID: { "phone-install-0123456789" }, environment: "sandbox", pause: { _ in Issue.record("Already unregistered"); throw CancellationError() })
        registrar.handle(.ready)
        registrar.set([
            ActivityRelayPairing(id: "a", register: { _ in }, unregister: { _ in
                removed.withLock { $0.append("a") }
                throw RelayWireError.unauthorized
            }),
            ActivityRelayPairing(id: "b", register: { _ in }, unregister: { _ in removed.withLock { $0.append("b") } }),
        ])
        await registrar.settled()
        registrar.handle(.enabled(false))
        await registrar.settled()
        #expect(registrar.relayState == .registered)
        registrar.retry()
        await registrar.settled()
        #expect(removed.withLock { $0 } == ["a", "b"])
    }

    @Test func theRelayAndKeyRecoverIndependentlyWithIncreasingWaits() async {
        let attempts = ActivityAttempts()
        let registrar = ActivityRegistrar(phoneID: { "phone-install-0123456789" }, environment: "sandbox", pause: { await attempts.pause($0) })
        registrar.handle(.ready)
        registrar.set([ActivityRelayPairing(id: "mac", register: { _ in try await attempts.register() }, unregister: { _ in },
            publicKey: "test-key", giveKey: { _ in try await attempts.giveKey() })])
        await registrar.settled()
        #expect(registrar.relayState == .registered)
        #expect(registrar.keyState == .registered)
        #expect(await attempts.relayCalls == 3)
        #expect(await attempts.keyCalls == 2)
        #expect(await attempts.waits.sorted() == [.seconds(5), .seconds(5), .seconds(10)])
    }

    @Test(arguments: [RegistrationFailure.Reason.approvalRequired, .incompatible, .invalidRequest(.invalidActivityKey)])
    func aKeyRejectionStaysVisibleWhileTheRelaySucceeds(reason: RegistrationFailure.Reason) async {
        let calls = Mutex(0)
        let registrar = ActivityRegistrar(phoneID: { "phone-install-0123456789" }, environment: "sandbox", pause: { _ in Issue.record("Permanent rejection retried"); throw CancellationError() })
        registrar.handle(.ready)
        let failure = RegistrationFailure(operation: .activityKey, reason: reason)
        registrar.set([ActivityRelayPairing(id: "mac", register: { _ in }, unregister: { _ in }, publicKey: "test-key", giveKey: { _ in
            calls.withLock { $0 += 1 }
            throw failure
        })])
        await registrar.settled()
        #expect(registrar.relayState == .registered)
        #expect(registrar.keyState == .failed([failure]))
        #expect(calls.withLock { $0 } == 1)
        registrar.set([ActivityRelayPairing(id: "mac", register: { _ in }, unregister: { _ in }, publicKey: "test-key", giveKey: { _ in })])
        await registrar.settled()
        #expect(registrar.keyState == .registered)
    }

    @Test(arguments: [true, false])
    func logoutCancelsPendingRelayAndKeyRetries(key: Bool) async {
        let gate = ActivityGate()
        let calls = Mutex(0)
        let failure = RegistrationFailure(operation: key ? .activityKey : .relay, reason: .unavailable)
        let fail: @Sendable () throws -> Void = { calls.withLock { $0 += 1 }; throw failure }
        let registrar = ActivityRegistrar(phoneID: { "phone-install-0123456789" }, environment: "sandbox", pause: { _ in await gate.suspend() })
        registrar.handle(.ready)
        registrar.set([ActivityRelayPairing(id: "mac", register: { _ in if !key { try fail() } }, unregister: { _ in },
            publicKey: "test-key", giveKey: { _ in if key { try fail() } })])
        await gate.waitUntilEntered()
        registrar.stop()
        await gate.resume()
        await registrar.settled()
        #expect(calls.withLock { $0 } == 1)
        #expect(registrar.relayState == .idle)
        #expect(registrar.keyState == .idle)
    }

    @Test func aLateKeySuccessCannotRegisterTheNextSessionOrForgetItsIdentity() async {
        let gate = ActivityGate()
        let calls = Mutex(0)
        let identity = Mutex("phone-install-old-0123456789")
        let unregistered = Mutex<[String]>([])
        let registrar = ActivityRegistrar(phoneID: { identity.withLock { $0 } }, forgetPhoneID: { identity.withLock { $0 = "phone-install-new-0123456789" } }, environment: "sandbox")
        let pairing = ActivityRelayPairing(id: "mac", register: { _ in }, unregister: { id in unregistered.withLock { $0.append(id) } }, publicKey: "test-key") { _ in
            let first = calls.withLock { $0 += 1; return $0 == 1 }
            if first { await gate.suspend() }
        }
        registrar.handle(.ready)
        registrar.set([pairing])
        await gate.waitUntilEntered()
        registrar.stop()
        registrar.set([pairing])
        await gate.resume()
        await registrar.settled()
        #expect(calls.withLock { $0 } == 2)
        #expect(unregistered.withLock { $0 } == ["phone-install-old-0123456789"])
        #expect(identity.withLock { $0 } == "phone-install-new-0123456789")
        #expect(registrar.keyState == .registered)
    }
}
