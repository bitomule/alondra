@testable import AlondraTransport
import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import CryptoKit
import XCTest

/// The iPhone's whole path to one paired Mac, from the screen down to the bytes: the real
/// RoutedSession, DirectSession and RelaySession contract, FeedSession and ScreenFeature, with
/// only the Mac's direct port and the relay replaced. Each carrier can be made to answer, be
/// slow, be refused by Local Network privacy, or lose the Mac.
@MainActor
final class ConnectionRecoveryTests: XCTestCase {
    func testADirectPathThatFailsHandsTheScreenToTheRelayWithoutShowingOffline() async throws {
        for failure in [Carrier.slow, .localNetworkDenied, .down] {
            let world = try await World.make()
            await world.mac.set(direct: failure)
            let store = world.screen()

            await store.send(.task)
            await store.receive(\.loaded) { $0.document = world.tasks; $0.phase = .loaded }
            let relayReads = await world.mac.relayViewReads
            XCTAssertEqual(relayReads, 1, "\(failure): the relay answered the read")
            if failure == .slow {
                let timeouts = await world.mac.directTimeouts
                XCTAssertEqual(timeouts, [8], "a slow Mac gets one short try before the relay")
            }
            await store.send(.stop)
            await world.end()
        }
    }

    func testAReadTheRelayCannotGetAnswersIsAskedDirectly() async throws {
        let world = try await World.make(provingDirect: false)
        await world.mac.set(relay: .slow)
        let store = world.screen()

        await store.send(.task)
        await store.receive(\.loaded) { $0.document = world.tasks; $0.phase = .loaded }
        await store.send(.stop)
        await world.end()
    }

    func testLosingTheMacWithTheAppOpenPicksUpItsChangesOnceItAnswersAgain() async throws {
        let world = try await World.make()
        let store = world.screen()
        await store.send(.task)
        await store.receive(\.loaded) { $0.document = world.tasks; $0.phase = .loaded }

        await world.mac.set(direct: .down)
        await world.mac.set(relay: .down)
        try await world.mac.waitForFailedPolls(2)
        let newer = try Fixtures.document("tasks-ios", version: "v2")
        try await world.mac.serve(newer, changed: ["tasks"])
        await world.mac.set(relay: .answers)

        await store.receive(\.loaded, timeout: .seconds(5)) { $0.document = newer }
        await store.send(.stop)
        await world.end()
    }

    func testAScreenThatCouldNotLoadRecoversOnItsOwnOnceTheMacAnswers() async throws {
        let world = try await World.make()
        await world.mac.set(direct: .down)
        await world.mac.set(relay: .down)
        let store = world.screen()

        await store.send(.task)
        await store.receive(\.loadFailed, timeout: .seconds(5)) { $0.failures = 1 }
        await world.mac.set(relay: .answers)
        await world.clock.advance(by: .seconds(1))
        await store.receive(\.retry)
        await store.receive(\.loaded, timeout: .seconds(5)) { $0.document = world.tasks; $0.phase = .loaded; $0.failures = 0 }
        await store.send(.stop)
        await world.end()
    }

    func testRetryingTheConnectionRetriesAFailedScreenAtOnce() async throws {
        let world = try await World.make()
        await world.mac.set(direct: .down)
        await world.mac.set(relay: .down)
        let store = world.screen()

        await store.send(.task)
        await store.receive(\.loadFailed, timeout: .seconds(5)) { $0.failures = 1 }
        await world.mac.set(direct: .answers)
        await world.routed.networkChanged()
        await world.feed.reconnect()
        await store.receive(\.retry)
        await store.receive(\.loaded, timeout: .seconds(5)) { $0.document = world.tasks; $0.phase = .loaded; $0.failures = 0 }
        await store.send(.stop)
        await world.end()
    }

    /// The one connection state follows the Mac with the app open: answering, gone once the
    /// changes poll keeps failing, and back as soon as the relay answers again.
    func testTheConnectionStateFollowsTheMacWithTheAppOpen() async throws {
        let world = try await World.make()
        let store = world.screen()
        await store.send(.task)
        await store.receive(\.loaded) { $0.document = world.tasks; $0.phase = .loaded }
        await world.connection(becomes: .connected(.localNetwork))

        await world.mac.set(direct: .down)
        await world.mac.set(relay: .down)
        await world.connection(becomes: .unreachable(.macNotAnswering))

        await world.mac.set(relay: .answers)
        await world.connection(becomes: .connected(.relay))
        await store.send(.stop)
        await world.end()
    }

    /// A screen whose load failed asks again the moment the Mac answers anything, instead of
    /// waiting out its backoff with saved data on screen.
    func testAFailedScreenRetriesAtOnceWhenTheMacAnswersAgain() async throws {
        let world = try await World.make()
        await world.mac.set(direct: .down)
        await world.mac.set(relay: .down)
        await world.monitor.handle(.networkLost)
        await world.monitor.handle(.networkBack)
        let store = world.screen()

        await store.send(.task)
        await store.receive(\.loadFailed, timeout: .seconds(5)) { $0.failures = 1 }
        await world.connection(becomes: .unreachable(.macNotAnswering))
        await world.mac.set(relay: .answers)
        let anotherRead = try APIRequest(endpoint: Endpoint<Data>.data(.get, path: ["capabilities"]), operationID: "other")
        _ = try await world.monitored.perform(anotherRead)
        await store.receive(\.retry, timeout: .seconds(5))
        await store.receive(\.loaded, timeout: .seconds(5)) { $0.document = world.tasks; $0.phase = .loaded; $0.failures = 0 }
        await store.send(.stop)
        await world.end()
    }
}

private enum Carrier: Equatable { case answers, slow, localNetworkDenied, down }

@MainActor
private struct World {
    let mac: SyntheticMac
    let routed: RoutedSession
    let feed: FeedSession
    let sender: ViewActionSender
    let monitor: ConnectionMonitor
    let monitored: MonitoredOperations
    let clock = TestClock()
    let tasks: ViewDocument

    /// The relay answered first and the background probe proved the direct path, so reads go
    /// direct: the state the iPhone was in on the same Wi-Fi as the Mac.
    static func make(provingDirect: Bool = true) async throws -> World {
        let tasks = try Fixtures.document("tasks-ios")
        let mac = try SyntheticMac(tasks: tasks)
        let http = DirectHTTP(transport: SyntheticDirectPort(mac: mac))
        let router = DirectRouter(pinned: mac.identity, saved: [mac.lan], http: http, network: { "home" })
        let direct = DirectSession(macID: "mac-1", deviceID: "phone-1", signing: mac.device,
            agreement: mac.deviceAgreement, macSigning: mac.macSigning.publicKey,
            macAgreement: mac.macAgreement.publicKey, router: router, http: http, pause: { _ in })
        let routed = RoutedSession(direct: direct, router: router, relay: SyntheticRelay(mac: mac))
        let monitor = ConnectionMonitor { _ in .macNotAnswering }
        let monitored = MonitoredOperations(routed, monitor: monitor) { await routed.route ?? .relay }
        let api = AlondraAPIClient(serviceURL: URL(string: "https://relay.test")!, deviceID: "phone-1", operations: monitored)
        let probe = try APIRequest(endpoint: Endpoint<Data>.data(.get, path: ["capabilities"]), operationID: "probe")
        if provingDirect {
            for _ in 0..<500 where await routed.route != .localNetwork {
                _ = try? await routed.perform(probe)
                try await Task.sleep(for: .milliseconds(10))
            }
            let route = await routed.route
            XCTAssertEqual(route, .localNetwork)
        }
        let feed = FeedSession(transport: api, client: .ios, lang: "en",
                               retryDelay: { _ in try await Task.sleep(for: .milliseconds(20)) })
        return World(mac: mac, routed: routed, feed: feed, sender: ViewActionSender(transport: api), monitor: monitor,
                     monitored: monitored, tasks: tasks)
    }

    func screen() -> TestStoreOf<ScreenFeature> {
        TestStore(initialState: ScreenFeature.State(key: .tasks)) { ScreenFeature() } withDependencies: {
            $0.viewsClient = .live(session: feed, sender: sender, monitor: monitor)
            $0.continuousClock = clock
            $0.withRandomNumberGenerator = WithRandomNumberGenerator(TopOfRange())
        }
    }

    /// Waits until the connection state is `expected`, failing after five seconds.
    func connection(becomes expected: ConnectionState) async {
        for _ in 0..<500 where await monitor.state != expected { try? await Task.sleep(for: .milliseconds(10)) }
        let state = await monitor.state
        XCTAssertEqual(state, expected)
    }

    func end() async {
        await monitor.stop()
        await feed.stop()
        await routed.deactivate()
    }
}

private struct TopOfRange: RandomNumberGenerator {
    mutating func next() -> UInt64 { .max }
}

/// The paired Mac: what it serves, which changes it has for the next poll, and how each
/// carrier reaches it right now.
private actor SyntheticMac {
    nonisolated let macSigning = P256.Signing.PrivateKey()
    nonisolated let macAgreement = P256.KeyAgreement.PrivateKey()
    nonisolated let device = P256.Signing.PrivateKey()
    nonisolated let deviceAgreement = P256.KeyAgreement.PrivateKey()
    nonisolated let lan = DirectEndpoint(host: "192.168.1.5", port: 47810, kind: .localNetwork)
    private(set) var direct = Carrier.answers
    private(set) var relay = Carrier.answers
    private(set) var directTimeouts: [TimeInterval] = []
    private(set) var relayViewReads = 0
    private var failedPolls = 0
    private var documents: [String: Data] = [:]
    private var pending: [String] = []

    init(tasks: ViewDocument) throws {
        documents["tasks"] = try ContractCodec.encode(tasks)
    }

    nonisolated var identity: DirectMacIdentity {
        DirectMacIdentity(macID: "mac-1", name: "Studio",
                          signingKey: Base64URL.encode(macSigning.publicKey.x963Representation),
                          agreementKey: Base64URL.encode(macAgreement.publicKey.x963Representation))
    }

    func set(direct: Carrier) { self.direct = direct }
    func set(relay: Carrier) { self.relay = relay }
    func recordTimeout(_ timeout: TimeInterval) { directTimeouts.append(timeout) }
    func countRelayViewRead() { relayViewReads += 1 }
    func countFailedPoll() { failedPolls += 1 }

    func serve(_ document: ViewDocument, changed keys: [String]) throws {
        documents[document.key] = try ContractCodec.encode(document)
        pending += keys
    }

    func waitForFailedPolls(_ count: Int) async throws {
        let target = failedPolls + count
        for _ in 0..<500 where failedPolls < target { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertGreaterThanOrEqual(failedPolls, target)
    }

    /// The Mac's answer to one request: a view, a changes poll (held open until something
    /// changed), or the capabilities the probe asks for.
    func answer(path: String, direct viaDirect: Bool) async throws -> Data {
        if path.hasPrefix("/views/") {
            let key = String(path.dropFirst("/views/".count).prefix { $0 != "?" })
            return try XCTUnwrap(documents[key])
        }
        if path.hasPrefix("/changes?") {
            if path.contains("wait=0"), (viaDirect ? direct : relay) == .down { throw URLError(.networkConnectionLost) }
            while pending.isEmpty, !path.contains("wait=0") {
                if (viaDirect ? direct : relay) == .down { throw URLError(.networkConnectionLost) }
                try await Task.sleep(for: .milliseconds(10))
            }
            let stale = pending
            pending = []
            return Data("""
            {"protocol":1,"cursor":"cursor-2","data_epoch":"test-epoch","status":{"version":"s","summary":"Ready","badge":0,
            "counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},
            "stale":\(String(decoding: try JSONEncoder().encode(stale), as: UTF8.self)),"reset":false,"superseded":false}
            """.utf8)
        }
        return Data("{}".utf8)
    }

    func sealed(_ body: Data, requestID: String) throws -> Data {
        let answer: [String: Any] = ["status": 200, "body": try JSONSerialization.jsonObject(with: body)]
        return try JSONEncoder().encode(Envelope.seal(route: "mac-1", sender: "mac-1", recipient: "phone-1",
            requestID: requestID, kind: "response",
            expiresAtMilliseconds: UInt64(Date().timeIntervalSince1970 * 1000) + 60_000,
            plaintext: JSONSerialization.data(withJSONObject: answer), senderKey: macSigning,
            recipientKey: deviceAgreement.publicKey))
    }

    func opened(_ body: Data) throws -> (requestID: String, path: String) {
        let envelope = try JSONDecoder().decode(Envelope.self, from: body)
        let plain = try envelope.open(senderKey: device.publicKey, recipientKey: macAgreement, sender: "phone-1",
                                      recipient: "mac-1", route: "mac-1", nowMilliseconds: nil)
        let request = try XCTUnwrap(JSONSerialization.jsonObject(with: plain) as? [String: Any])
        return (envelope.requestID, try XCTUnwrap(request["path"] as? String))
    }
}

/// iOS refuses a local address this way when Local Network access is off for the app.
private let localNetworkProhibited = URLError(.notConnectedToInternet,
    userInfo: ["_NSURLErrorNWPathKey": "unsatisfied (Local network prohibited), interface: en0[802.11]"])

/// The Mac's direct port on the LAN.
private struct SyntheticDirectPort: HTTPTransport {
    let mac: SyntheticMac

    func send(_ request: URLRequest) async throws -> HTTPReply {
        let carrier = await mac.direct
        switch carrier {
        case .down: throw URLError(.cannotConnectToHost)
        case .localNetworkDenied: throw localNetworkProhibited
        case .answers, .slow: break
        }
        if request.url?.path == "/direct/hello" {
            return HTTPReply(status: 200, body: try JSONEncoder().encode(mac.identity))
        }
        let (requestID, path) = try await mac.opened(try XCTUnwrap(request.httpBody))
        if carrier == .slow {
            await mac.recordTimeout(request.timeoutInterval)
            throw URLError(.timedOut)
        }
        return HTTPReply(status: 200, body: try await mac.sealed(try await mac.answer(path: path, direct: true), requestID: requestID))
    }
}

/// The relay: when it cannot reach the Mac it answers that the outcome is unknown (a read) or
/// fails with it, the way RelaySession does once its deadline passes.
private struct SyntheticRelay: RelayOperations {
    let mac: SyntheticMac

    func perform(_ request: APIRequest) async throws -> APIResponse {
        switch await mac.relay {
        case .down:
            if request.path.hasPrefix("/changes?") { await mac.countFailedPoll() }
            throw RelaySessionError.outcomeUnknown
        case .slow: return .unknown
        case .answers, .localNetworkDenied:
            if request.path.hasPrefix("/views/") { await mac.countRelayViewRead() }
            do { return .completed(status: 200, body: try await mac.answer(path: request.path, direct: false)) }
            catch is URLError { throw RelaySessionError.outcomeUnknown }
        }
    }

    func sentEnvelope(for requestID: String) async -> Envelope? { nil }
    func registerServicePush(token: String, environment: String, binding: String) async throws {}
    func deactivate() async {}
}
