@testable import AlondraTransport
import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import CryptoKit
import Network
import Synchronization
import XCTest

/// The iPhone's whole path to one paired Mac, from the screen down to the bytes: the real
/// RoutedSession, IrohCarrier and DirectSession, FeedSession and ScreenFeature, with only the
/// Mac's iroh endpoint and direct port replaced. Each carrier can be made to answer, be slow, be
/// refused by Local Network privacy, or lose the Mac.
@MainActor
final class ConnectionRecoveryTests: XCTestCase {
    func testWhileIrohAnswersAFailingLocalNetworkNeverReachesTheScreen() async throws {
        for failure in [Carrier.slow, .localNetworkDenied, .down] {
            let world = try await World.make()
            await world.mac.set(direct: failure)
            let store = world.screen()

            await store.send(.task)
            await store.receive(\.loaded) { $0.document = world.tasks; $0.phase = .loaded }
            let irohReads = await world.mac.irohViewReads
            XCTAssertEqual(irohReads, 1, "\(failure): iroh answered the read")
            let directRequests = await world.mac.directRequests
            XCTAssertEqual(directRequests, 0, "\(failure): the local network was never asked")
            await store.send(.stop)
            await world.end()
        }
    }

    func testWhenIrohCannotConnectTheLocalNetworkAnswers() async throws {
        let world = try await World.make()
        await world.mac.set(iroh: .down)
        let store = world.screen()

        await store.send(.task)
        await store.receive(\.loaded) { $0.document = world.tasks; $0.phase = .loaded }
        await world.connection(becomes: .connected(.localNetwork))
        await store.send(.stop)
        await world.end()
    }

    func testAReadIrohGetsNoAnswerToIsAskedDirectly() async throws {
        let world = try await World.make()
        await world.mac.set(iroh: .slow)
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
        await world.mac.set(iroh: .down)
        try await world.mac.waitForFailedPolls(2)
        let newer = try Fixtures.document("tasks-ios", version: "v2")
        try await world.mac.serve(newer, changed: ["tasks"])
        await world.mac.set(iroh: .answers)

        await store.receive(\.loaded, timeout: .seconds(5)) { $0.document = newer }
        await store.send(.stop)
        await world.end()
    }

    func testAScreenThatCouldNotLoadRecoversOnItsOwnOnceTheMacAnswers() async throws {
        let world = try await World.make()
        await world.mac.set(direct: .down)
        await world.mac.set(iroh: .down)
        let store = world.screen()

        await store.send(.task)
        await store.receive(\.loadFailed, timeout: .seconds(5)) { $0.failures = 1 }
        await world.mac.set(iroh: .answers)
        await world.clock.advance(by: .seconds(1))
        await store.receive(\.retry)
        await store.receive(\.loaded, timeout: .seconds(5)) { $0.document = world.tasks; $0.phase = .loaded; $0.failures = 0 }
        await store.send(.stop)
        await world.end()
    }

    func testRetryingTheConnectionRetriesAFailedScreenAtOnce() async throws {
        let world = try await World.make()
        await world.mac.set(direct: .down)
        await world.mac.set(iroh: .down)
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

    /// The one connection state follows the Mac with the app open: answering over iroh, gone once
    /// the changes poll keeps failing, and back as soon as any path answers again.
    func testTheConnectionStateFollowsTheMacWithTheAppOpen() async throws {
        let world = try await World.make()
        let store = world.screen()
        await store.send(.task)
        await store.receive(\.loaded) { $0.document = world.tasks; $0.phase = .loaded }
        await world.connection(becomes: .connected(.irohRelay))

        await world.mac.set(direct: .down)
        await world.mac.set(iroh: .down)
        await world.connection(becomes: .unreachable(.macNotAnswering))

        await world.mac.set(iroh: .answers)
        await world.connection(becomes: .connected(.irohRelay))
        await store.send(.stop)
        await world.end()
    }

    /// A screen whose load failed asks again the moment the Mac answers anything, instead of
    /// waiting out its backoff with saved data on screen.
    func testAFailedScreenRetriesAtOnceWhenTheMacAnswersAgain() async throws {
        let world = try await World.make()
        await world.mac.set(direct: .down)
        await world.mac.set(iroh: .down)
        await world.monitor.handle(.networkLost)
        await world.monitor.handle(.networkBack)
        let store = world.screen()

        await store.send(.task)
        await store.receive(\.loadFailed, timeout: .seconds(5)) { $0.failures = 1 }
        await world.connection(becomes: .unreachable(.macNotAnswering))
        await world.mac.set(iroh: .answers)
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

    /// A Mac paired with an account, with Local Network access allowed: iroh first, then the LAN.
    static func make() async throws -> World {
        let tasks = try Fixtures.document("tasks-ios")
        let mac = try SyntheticMac(tasks: tasks)
        let http = DirectHTTP(transport: SyntheticDirectPort(mac: mac), localNetwork: LocalNetworkProbe { _ in
            await mac.direct == .localNetworkDenied ? .localNetworkDenied : .notAvailable
        })
        let router = DirectRouter(pinned: mac.identity, saved: [mac.lan], http: http, network: { "home" })
        let direct = DirectSession(macID: "mac-1", deviceID: "phone-1", signing: mac.device,
            agreement: mac.deviceAgreement, macSigning: mac.macSigning.publicKey,
            macAgreement: mac.macAgreement.publicKey, router: router, http: http, pause: { _ in })
        let endpoint = IrohIdentity(seed: Data(repeating: 3, count: 32)).endpointIDHex
        let carrier = IrohCarrier(dialer: SyntheticIrohDialer(mac: mac), macID: "mac-1", deviceID: "phone-1",
            signing: mac.device, address: { IrohMacAddress(endpointID: endpoint, relay: URL(string: "https://iroh.test")) },
            retryDelays: [.milliseconds(20)])
        let iroh = RoutedSession.Iroh(session: DirectSession(macID: "mac-1", deviceID: "phone-1", signing: mac.device,
            agreement: mac.deviceAgreement, macSigning: mac.macSigning.publicKey, macAgreement: mac.macAgreement.publicKey,
            iroh: carrier), carrier: carrier)
        let routed = RoutedSession(iroh: iroh, direct: direct, router: router)
        await routed.goLive()
        let monitor = ConnectionMonitor { _ in .macNotAnswering }
        let monitored = MonitoredOperations(PollCounter(inner: routed, mac: mac), monitor: monitor) {
            await routed.route ?? .localNetwork
        }
        let api = AlondraAPIClient(serviceURL: URL(string: "https://relay.test")!, deviceID: "phone-1", operations: monitored)
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
    private(set) var iroh = Carrier.answers
    private(set) var directRequests = 0
    private(set) var irohViewReads = 0
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
    func set(iroh: Carrier) { self.iroh = iroh }
    func countDirectRequest() { directRequests += 1 }
    func countIrohViewRead() { irohViewReads += 1 }
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
            if path.contains("wait=0"), (viaDirect ? direct : iroh) == .down { throw URLError(.networkConnectionLost) }
            while pending.isEmpty, !path.contains("wait=0") {
                if (viaDirect ? direct : iroh) == .down { throw URLError(.networkConnectionLost) }
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


/// The Mac's direct port on the LAN.
private struct SyntheticDirectPort: HTTPTransport {
    let mac: SyntheticMac

    func send(_ request: URLRequest) async throws -> HTTPReply {
        let carrier = await mac.direct
        switch carrier {
        case .down: throw URLError(.cannotConnectToHost)
        case .localNetworkDenied: throw URLError(.notConnectedToInternet)
        case .answers, .slow: break
        }
        if request.url?.path == "/direct/hello" {
            return HTTPReply(status: 200, body: try JSONEncoder().encode(mac.identity))
        }
        await mac.countDirectRequest()
        let (requestID, path) = try await mac.opened(try XCTUnwrap(request.httpBody))
        if carrier == .slow { throw URLError(.timedOut) }
        return HTTPReply(status: 200, body: try await mac.sealed(try await mac.answer(path: path, direct: true), requestID: requestID))
    }
}

/// The Mac's iroh endpoint: a connection is refused while it is down, and one already open is
/// lost with it.
private struct SyntheticIrohDialer: IrohDialer {
    let mac: SyntheticMac
    let endpointID = IrohIdentity(seed: Data(repeating: 4, count: 32)).endpointID

    func dial(_ address: IrohMacAddress) async throws -> any IrohLink {
        guard await mac.iroh != .down else { throw URLError(.cannotConnectToHost) }
        return SyntheticIrohLink(mac: mac)
    }

    func networkMoved() async {}
    func shutdown() async {}
}

private final class SyntheticIrohLink: IrohLink {
    private let mac: SyntheticMac
    private let state = Mutex((hello: false, closed: false))

    init(mac: SyntheticMac) { self.mac = mac }

    func exchange(_ request: Data, answerLimit: Int) async throws -> Data {
        guard !isClosed else { throw IrohLinkError.notSent }
        let hello = state.withLock { state in
            defer { state.hello = true }
            return !state.hello
        }
        if hello { return Data(#"{"ok":true}"#.utf8) }
        switch await mac.iroh {
        case .down:
            await close()
            throw IrohLinkError.notSent
        case .slow, .localNetworkDenied:
            throw IrohLinkError.noAnswer
        case .answers:
            break
        }
        let (requestID, path) = try await mac.opened(request)
        if path.hasPrefix("/views/") { await mac.countIrohViewRead() }
        do {
            let sealed = try await mac.sealed(try await mac.answer(path: path, direct: false), requestID: requestID)
            return Data(#"{"response":"#.utf8) + sealed + Data("}".utf8)
        } catch is URLError {
            await close()
            throw IrohLinkError.noAnswer
        }
    }

    func path() -> IrohPath { .relay }
    func pathChanges() -> AsyncStream<IrohPath> { AsyncStream { _ in } }
    var isClosed: Bool { state.withLock { $0.closed } }
    func close() async { state.withLock { $0.closed = true } }
}

/// Counts the changes polls that failed, whichever carrier they went through.
private struct PollCounter: OperationAPI {
    let inner: RoutedSession
    let mac: SyntheticMac

    func perform(_ request: APIRequest) async throws -> APIResponse {
        do { return try await inner.perform(request) }
        catch {
            if request.path.hasPrefix("/changes?") { await mac.countFailedPoll() }
            throw error
        }
    }
    func recover(_ request: APIRequest) async throws -> APIResponse { try await inner.recover(request) }
    func resend(_ request: APIRequest) async throws -> APIResponse { try await inner.resend(request) }
    func lookup(_ request: APIRequest) async throws -> OperationLookup { try await inner.lookup(request) }
    func retireActions(_ acknowledgment: ActionJournalAcknowledgment) async throws { try await inner.retireActions(acknowledgment) }
}
