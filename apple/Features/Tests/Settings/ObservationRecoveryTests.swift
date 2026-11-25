import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

/// A screen that keeps its view with a `.task` observation must recover when the first observation
/// fails because the connection is not ready yet, instead of staying empty until it is reopened.
@MainActor
final class ObservationRecoveryTests: XCTestCase {
    private struct NotReady: Error {}

    /// Fails the first observation the way a connection that is not ready does, then serves `document`.
    private struct Source: Sendable {
        let attempts = LockIsolated(0)
        let document: ViewDocument
        var failing = 1

        func observe(_: ViewKey) -> AsyncThrowingStream<ViewFetch, any Error> {
            let attempt = attempts.withValue { $0 += 1; return $0 }
            return AsyncThrowingStream { continuation in
                if attempt <= failing {
                    continuation.finish(throwing: NotReady())
                } else {
                    continuation.yield(.document(document))
                }
            }
        }
    }

    private func metrics() throws -> ViewDocument {
        let environment = ProcessInfo.processInfo.environment
        let url = URL(fileURLWithPath: try XCTUnwrap(environment["TEST_SRCDIR"]))
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views/metrics/metrics-ios.json")
        return try ContractCodec.decode(ViewDocument.self, from: Data(contentsOf: url))
    }

    private func settingsStore(
        _ source: Source, reconnections: AsyncStream<Void> = AsyncStream { _ in }, clock: TestClock<Duration> = TestClock()
    ) -> TestStoreOf<ServerSettingsFeature> {
        let store = TestStore(initialState: ServerSettingsFeature.State(key: .mcpServers, client: .ios, lang: "es")) {
            ServerSettingsFeature()
        } withDependencies: {
            $0.viewsClient.observe = source.observe
            $0.viewsClient.reconnections = { reconnections }
            $0.continuousClock = clock
            $0.withRandomNumberGenerator = WithRandomNumberGenerator(Ceiling())
        }
        store.exhaustivity = .off
        return store
    }

    func testSettingsAskForTheViewAgainWhenTheAppRetriesTheConnectionAfterTheFirstObservationFailed() async throws {
        let document = try Fixtures.document("mcp-servers-ios-es")
        let source = Source(document: document)
        let (reconnections, reconnect) = AsyncStream<Void>.makeStream()
        let store = settingsStore(source, reconnections: reconnections)

        await store.send(.task)
        XCTAssertNil(store.state.document, "the first observation failed: nothing to show yet")
        reconnect.yield()
        await store.receive(\.loaded)
        XCTAssertEqual(store.state.document, document, "the Accounts section comes back without reopening Settings")
        XCTAssertEqual(source.attempts.value, 2)
        await store.send(.stop)
        await store.finish()
    }

    func testSettingsAskForTheViewAgainOnTheirOwnWhenNothingTellsThemTheConnectionIsReady() async throws {
        let document = try Fixtures.document("mcp-servers-ios-es")
        let source = Source(document: document, failing: 2)
        let clock = TestClock()
        let store = settingsStore(source, clock: clock)

        await store.send(.task)
        await clock.advance(by: .seconds(1))
        await clock.advance(by: .seconds(2))
        await store.receive(\.loaded)
        XCTAssertEqual(store.state.document, document)
        XCTAssertEqual(source.attempts.value, 3, "each wait is longer than the last: 1 s, then 2 s")
        await store.send(.stop)
        await store.finish()
    }

    func testSettingsAskForTheViewAgainWhenTheMacStartsAnsweringAfterTheFailure() async throws {
        let document = try Fixtures.document("mcp-servers-ios-es")
        let source = Source(document: document)
        let (connection, change) = AsyncStream<ConnectionState>.makeStream()
        let store = TestStore(initialState: ServerSettingsFeature.State(key: .mcpServers, client: .ios, lang: "es")) {
            ServerSettingsFeature()
        } withDependencies: {
            $0.viewsClient.observe = source.observe
            $0.viewsClient.connection = { connection }
            $0.continuousClock = TestClock()
            $0.withRandomNumberGenerator = WithRandomNumberGenerator(Ceiling())
        }
        store.exhaustivity = .off

        await store.send(.task)
        change.yield(.connecting)
        change.yield(.connected(.irohRelay))
        await store.receive(\.loaded)
        XCTAssertEqual(store.state.document, document)
        await store.send(.stop)
        await store.finish()
    }

    func testAnObservationThatEndsWithoutAnErrorIsObservedAgain() async throws {
        let document = try Fixtures.document("mcp-servers-ios-es")
        let attempts = LockIsolated(0)
        let (reconnections, reconnect) = AsyncStream<Void>.makeStream()
        let store = TestStore(initialState: ServerSettingsFeature.State(key: .mcpServers, client: .ios, lang: "es")) {
            ServerSettingsFeature()
        } withDependencies: {
            $0.viewsClient.observe = { _ in
                let attempt = attempts.withValue { $0 += 1; return $0 }
                return AsyncThrowingStream { continuation in
                    if attempt == 1 { continuation.finish() } else { continuation.yield(.document(document)) }
                }
            }
            $0.viewsClient.reconnections = { reconnections }
            $0.continuousClock = TestClock()
            $0.withRandomNumberGenerator = WithRandomNumberGenerator(Ceiling())
        }
        store.exhaustivity = .off

        await store.send(.task)
        reconnect.yield()
        await store.receive(\.loaded)
        XCTAssertEqual(store.state.document, document)
        await store.send(.stop)
        await store.finish()
    }

    func testTwoServerSettingsSectionsOnOneScreenEachKeepTheirOwnObservation() async throws {
        let document = try Fixtures.document("mcp-servers-ios-es")
        let (accounts, serveAccounts) = AsyncThrowingStream<ViewFetch, any Error>.makeStream()
        let (servers, serveServers) = AsyncThrowingStream<ViewFetch, any Error>.makeStream()
        let store = TestStore(initialState: SettingsPair.State()) { SettingsPair() } withDependencies: {
            $0.viewsClient.observe = { $0 == .accounts ? accounts : servers }
        }
        store.exhaustivity = .off

        await store.send(.accounts(.task))
        await store.send(.mcpServers(.task))
        serveAccounts.yield(.document(document))
        serveServers.yield(.document(document))
        await store.receive(\.accounts.loaded)
        await store.receive(\.mcpServers.loaded)
        XCTAssertEqual(store.state.accounts.document, document, "starting the MCP servers' observation must not end the Accounts one")
        XCTAssertEqual(store.state.mcpServers.document, document)
        await store.send(.accounts(.stop))
        await store.send(.mcpServers(.stop))
        await store.finish()
    }

    func testLeavingSettingsStopsTheWaitForTheConnection() async throws {
        let source = Source(document: try Fixtures.document("mcp-servers-ios-es"))
        let clock = TestClock()
        let store = settingsStore(source, clock: clock)

        await store.send(.task)
        await store.send(.stop)
        await store.finish()
        await clock.advance(by: .seconds(60))
        XCTAssertEqual(source.attempts.value, 1, "a screen that left never observes again")
    }

    func testTheMachineSettingsRecoverAfterTheFirstObservationFailed() async throws {
        let document = try Fixtures.document("machine-settings-ios-es")
        let source = Source(document: document)
        let clock = TestClock()
        let store = TestStore(initialState: MachineSettingsFeature.State(client: .ios, lang: "es")) {
            MachineSettingsFeature()
        } withDependencies: {
            $0.viewsClient.observe = source.observe
            $0.continuousClock = clock
            $0.withRandomNumberGenerator = WithRandomNumberGenerator(Ceiling())
        }
        store.exhaustivity = .off

        await store.send(.task)
        XCTAssertTrue(store.state.settings.isEmpty)
        await clock.advance(by: .seconds(1))
        await store.receive(\.loaded)
        XCTAssertEqual(store.state.settings.map(\.kind), [.mergeMethod, .branchUpdate, .nodeLimit])
        await store.send(.stop)
        await store.finish()
    }

    func testTheAllowedRulesRecoverAfterTheFirstObservationFailed() async throws {
        let document = try Fixtures.document("machine-settings-ios-es")
        let source = Source(document: document)
        let clock = TestClock()
        let store = TestStore(initialState: AllowedRulesFeature.State(client: .ios, lang: "es")) {
            AllowedRulesFeature()
        } withDependencies: {
            $0.viewsClient.observe = source.observe
            $0.continuousClock = clock
            $0.withRandomNumberGenerator = WithRandomNumberGenerator(Ceiling())
        }
        store.exhaustivity = .off

        await store.send(.task)
        XCTAssertTrue(store.state.rules.isEmpty)
        await clock.advance(by: .seconds(1))
        await store.receive(\.loaded)
        XCTAssertEqual(store.state.rules.map(\.id), ["rule-3", "rule-4"])
        await store.send(.stop)
        await store.finish()
    }

    func testTheMetricsScreenRecoversAfterTheFirstObservationFailed() async throws {
        let document = try metrics()
        let source = Source(document: document)
        let clock = TestClock()
        let store = TestStore(initialState: MetricsFeature.State(key: ViewKey(rawValue: document.key))) { MetricsFeature() } withDependencies: {
            $0.viewsClient.observe = source.observe
            $0.continuousClock = clock
            $0.withRandomNumberGenerator = WithRandomNumberGenerator(Ceiling())
        }
        store.exhaustivity = .off

        await store.send(.task)
        XCTAssertNil(store.state.document)
        await clock.advance(by: .seconds(1))
        await store.receive(\.loaded)
        XCTAssertEqual(store.state.document, document)
        await store.send(.stop)
        await store.finish()
    }

    func testAKeyTheMacDoesNotServeIsNotObservedAgain() async throws {
        let attempts = LockIsolated(0)
        let store = TestStore(initialState: MetricsFeature.State()) { MetricsFeature() } withDependencies: {
            $0.viewsClient.observe = { _ in
                attempts.withValue { $0 += 1 }
                return AsyncThrowingStream { $0.finish(throwing: ViewsClientError.missingView) }
            }
            $0.continuousClock = TestClock()
        }
        await store.send(.task)
        await store.receive(.missing)
        await store.finish()
        XCTAssertEqual(attempts.value, 1)
    }
}

/// The part of Settings that shows two server-drawn views at once, as the real screen does.
@Reducer
private struct SettingsPair {
    @ObservableState
    struct State: Equatable {
        var accounts = ServerSettingsFeature.State(key: .accounts)
        var mcpServers = ServerSettingsFeature.State(key: .mcpServers)
    }

    enum Action {
        case accounts(ServerSettingsFeature.Action)
        case mcpServers(ServerSettingsFeature.Action)
    }

    var body: some ReducerOf<Self> {
        Scope(state: \.accounts, action: \.accounts) { ServerSettingsFeature() }
        Scope(state: \.mcpServers, action: \.mcpServers) { ServerSettingsFeature() }
    }
}

/// Always the top of the jitter range, so each backoff wait is its full ceiling.
private struct Ceiling: RandomNumberGenerator {
    mutating func next() -> UInt64 { .max }
}
