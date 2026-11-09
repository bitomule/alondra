import AlondraLiveActivity
import AlondraTransport
import Foundation
import Synchronization
import Testing

private final class Relay: Sendable {
    let registered = Mutex<[String: [RelayActivityRegistration]]>([:])
    let unregistered = Mutex<[String]>([])
    let failing = Mutex(false)

    func pairing(_ id: String) -> ActivityRelayPairing {
        ActivityRelayPairing(id: id, register: { [self] registration in
            if failing.withLock({ $0 }) { throw URLError(.notConnectedToInternet) }
            registered.withLock { $0[id, default: []].append(registration) }
        }, unregister: { [self] _ in
            if failing.withLock({ $0 }) { throw URLError(.notConnectedToInternet) }
            unregistered.withLock { $0.append(id) }
        })
    }

    func sent(to id: String) -> [RelayActivityRegistration] { registered.withLock { $0[id] ?? [] } }
}

private final class Forgetful: Sendable {
    let forgotten = Mutex(false)
}

@MainActor
@Suite struct ActivityRegistrarTests {
    private let relay = Relay()
    private let forgetful = Forgetful()
    private let registrar: ActivityRegistrar
    private let startToken = String(repeating: "ab", count: 80)
    private let updateToken = String(repeating: "cd", count: 80)

    init() {
        let forgetful = forgetful
        registrar = ActivityRegistrar(phoneID: { "phone-install-0123456789" }, forgetPhoneID: { forgetful.forgotten.withLock { $0 = true } },
                                      environment: "sandbox", pause: { _ in throw CancellationError() })
        registrar.handle(.ready)
    }

    @Test func everyPairingHearsEachChangeOnce() async {
        registrar.set([relay.pairing("a"), relay.pairing("b")])
        await registrar.settled()
        registrar.handle(.startToken(startToken))
        await registrar.settled()
        registrar.retry()
        await registrar.settled()
        for pairing in ["a", "b"] {
            #expect(relay.sent(to: pairing).map(\.startToken) == [nil, startToken])
        }
    }

    @Test func theActivityTravelsWithItsTokenAndLeavesWhenItEnds() async {
        registrar.set([relay.pairing("a")])
        let started = Date(timeIntervalSince1970: 1_800_000_000)
        registrar.handle(.updateToken(updateToken, activityID: "activity", startedAt: started))
        await registrar.settled()
        let sent = relay.sent(to: "a").last
        #expect(sent?.updateToken == updateToken)
        #expect(sent?.activityID == "activity")
        #expect(sent?.startedAtMs == 1_800_000_000_000)
        registrar.handle(.activityEnded("another"))
        await registrar.settled()
        #expect(relay.sent(to: "a").last?.updateToken == updateToken)
        registrar.handle(.activityEnded("activity"))
        await registrar.settled()
        #expect(relay.sent(to: "a").last?.updateToken == nil)
    }

    @Test func theRelayHearsWhenFrequentUpdatesAreOff() async {
        registrar.set([relay.pairing("a")])
        registrar.handle(.frequentUpdates(false))
        await registrar.settled()
        #expect(relay.sent(to: "a").last?.frequentUpdates == false)
    }

    @Test func aFailedPairingIsToldAgainOnRetry() async {
        relay.failing.withLock { $0 = true }
        registrar.set([relay.pairing("a")])
        await registrar.settled()
        #expect(relay.sent(to: "a").isEmpty)
        relay.failing.withLock { $0 = false }
        registrar.retry()
        await registrar.settled()
        #expect(relay.sent(to: "a").count == 1)
    }

    @Test func turningLiveActivitiesOffLeavesTheRelayOnceAndBackOnReturns() async {
        registrar.set([relay.pairing("a"), relay.pairing("b")])
        await registrar.settled()
        registrar.handle(.enabled(false))
        await registrar.settled()
        registrar.retry()
        await registrar.settled()
        #expect(relay.unregistered.withLock { $0 } == ["a"])
        registrar.handle(.enabled(true))
        await registrar.settled()
        #expect(relay.sent(to: "b").count == 2)
    }

    @Test func nothingReachesTheRelayBeforeWhatIOSKnewAtLaunch() async {
        let launching = ActivityRegistrar(phoneID: { "phone-install-0123456789" }, environment: "sandbox")
        launching.set([relay.pairing("a")])
        launching.handle(.enabled(true))
        await launching.settled()
        #expect(relay.sent(to: "a").isEmpty)
        launching.handle(.startToken(startToken))
        launching.handle(.ready)
        await launching.settled()
        #expect(relay.sent(to: "a").map(\.startToken) == [startToken])
    }

    @Test func theRelayKeepsTheNewestActivitysTokenWhileADuplicateEnds() async {
        registrar.set([relay.pairing("a")])
        registrar.handle(.updateToken(updateToken, activityID: "new", startedAt: Date(timeIntervalSince1970: 2_000)))
        registrar.handle(.updateToken(startToken, activityID: "old", startedAt: Date(timeIntervalSince1970: 1_000)))
        await registrar.settled()
        #expect(relay.sent(to: "a").last?.updateToken == updateToken)
        registrar.handle(.activityEnded("old"))
        await registrar.settled()
        #expect(relay.sent(to: "a").last?.updateToken == updateToken)
    }

    @Test func theMacHearsEachKeyOnceAndAgainAfterItFailed() async {
        let given = Mutex<[String]>([])
        let failing = Mutex(true)
        let pairing = ActivityRelayPairing(id: "a", register: { _ in }, unregister: { _ in }, publicKey: "public") { key in
            if failing.withLock({ $0 }) { throw URLError(.notConnectedToInternet) }
            given.withLock { $0.append(key) }
        }
        registrar.set([pairing])
        await registrar.settled()
        #expect(given.withLock { $0 }.isEmpty)
        failing.withLock { $0 = false }
        registrar.retry()
        await registrar.settled()
        registrar.retry()
        await registrar.settled()
        #expect(given.withLock { $0 } == ["public"])
    }

    @Test func theLastPairingGoingLeavesTheRelayThroughIt() async {
        registrar.set([relay.pairing("a")])
        await registrar.settled()
        registrar.set([])
        await registrar.settled()
        #expect(relay.unregistered.withLock { $0 } == ["a"])
        #expect(forgetful.forgotten.withLock { $0 }, "the next account registers as another phone")
    }
}
