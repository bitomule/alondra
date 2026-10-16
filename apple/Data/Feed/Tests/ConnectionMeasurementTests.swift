import AlondraContract
import AlondraTestSupport
@testable import AlondraTransport
@testable import AlondraFeed
import Foundation
import Synchronization
import Testing

@Suite(.temporaryFiles)
struct ConnectionMeasurementTests {
    @Test func anAnsweredRequestRecordsItsRouteAndTime() async throws {
        let (operations, observed) = make([.success(.completed(status: 200, body: Data()))], route: .irohRelay)
        let request = try APIRequest(endpoint: Endpoint<Data>.data(.get, path: ["capabilities"]), operationID: "read-1")
        _ = try await operations.perform(request)
        #expect(observed.withLock { $0 } == [RequestObservation(operation: "perform", method: "GET", kind: "capabilities",
            route: .irohRelay, duration: .milliseconds(120), outcome: .ok)])
    }

    @Test func theMacsOwnErrorKeepsItsStatus() async throws {
        let (operations, observed) = make([.success(.completed(status: 409, body: Data()))], route: .localNetwork)
        let request = try APIRequest(endpoint: Endpoint<Data>.data(.get, path: ["capabilities"]), operationID: "read-1")
        _ = try await operations.perform(request)
        let only = try #require(observed.withLock { $0 }.first)
        #expect(only.outcome == .macError)
        #expect(only.status == 409)
        #expect(only.route == .localNetwork)
    }

    @Test func aFailureRecordsItsLabelAndNoRoute() async throws {
        let (operations, observed) = make([.failure(URLError(.timedOut))], route: .irohRelay)
        let request = try APIRequest(endpoint: Endpoint<Data>.data(.get, path: ["capabilities"]), operationID: "read-1")
        _ = try? await operations.perform(request)
        #expect(observed.withLock { $0 }.map(\.route) == [nil])
        #expect(observed.withLock { $0 }.map(\.outcome) == [.failed])
        #expect(observed.withLock { $0 }.map(\.error) == ["URLError(-1001)"])
    }

    @Test func aRequestTheMacNeverAnsweredIsUnanswered() async throws {
        let (operations, observed) = make([.success(.unknown)], route: .irohRelay)
        let request = try APIRequest(endpoint: Endpoint<Data>.data(.get, path: ["capabilities"]), operationID: "read-1")
        _ = try await operations.perform(request)
        #expect(observed.withLock { $0 }.map(\.outcome) == [.unanswered])
        #expect(observed.withLock { $0 }.map(\.route) == [nil])
    }

    @Test func nothingIdentifyingIsKept() async throws {
        #expect(RequestObservation.kind(of: "/views/t-secret-task/answer?token=abc") == "views")
        #expect(RequestObservation.kind(of: "/changes?since=4") == "changes")
        #expect(RequestObservation.kind(of: "/Secret%20Name") == "other")
        let server = APIError.server(status: 500, kind: "boom", detail: "/Users/david/private", reasons: [])
        #expect(RequestObservation.label(server) == "APIError.server(500)")
        let mac = ConnectionMeasurement.macReference("mac-1234-private-id")
        #expect(mac.count == 6)
        #expect(!mac.contains("mac-1234"))
        #expect(ConnectionMeasurement.macReference(nil) == "this-mac")
    }

    @Test func theStoreKeepsTheLatestAndReopensThem() async throws {
        let file = try TestFiles.directory().appending(path: "connection-measurements.jsonl")
        let store = ConnectionMeasurements(file: file, limit: 3, writesLog: false)
        for ms in 1...10 { await store.record(sample(ms: ms)) }
        #expect(await store.all().map(\.ms) == [8, 9, 10])
        let lines = try String(contentsOf: file, encoding: .utf8).split(separator: "\n")
        #expect(lines.count <= 4)
        let reopened = ConnectionMeasurements(file: file, limit: 3, writesLog: false)
        #expect(await reopened.all().map(\.ms) == [8, 9, 10])
        await reopened.clear()
        #expect(await ConnectionMeasurements(file: file, limit: 3, writesLog: false).all().isEmpty)
    }

    @Test func aSavedLineCarriesOnlyTheMeasuredFields() async throws {
        let file = try TestFiles.directory().appending(path: "connection-measurements.jsonl")
        let store = ConnectionMeasurements(file: file, writesLog: false)
        await store.record(sample(ms: 87, radio: "NR"))
        let line = try String(contentsOf: file, encoding: .utf8).trimmingCharacters(in: .newlines)
        let object = try #require(try JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any])
        #expect(Set(object.keys) == ["at", "mac", "operation", "method", "kind", "route", "ms", "network", "radio", "outcome"])
    }

    @Test func theSummaryKeepsLiveUpdateWaitsApart() throws {
        let measurements = [40, 50, 60, 70, 500].map { sample(ms: $0) }
            + [sample(ms: 0, outcome: .failed, route: nil)]
            + [sample(ms: 25_000, kind: "changes")]
            + [sample(ms: 1, outcome: .cancelled)]
        let summary = ConnectionMeasurementSummary.groups(measurements)
        #expect(summary.map(\.group) == [.requests, .requests, .liveUpdates])
        let relay = try #require(summary.first { $0.route == .irohRelay && $0.group == .requests })
        #expect(relay.count == 5)
        #expect(relay.medianMs == 60)
        #expect(relay.p90Ms == 500)
        #expect(summary.first { $0.route == nil }?.failures == 1)
        #expect(summary.first { $0.group == .liveUpdates }?.medianMs == 25_000)
    }

    private func sample(ms: Int, kind: String = "views", outcome: ConnectionMeasurement.Outcome = .ok,
                        route: ConnectionRoute? = .irohRelay, radio: String? = nil) -> ConnectionMeasurement {
        ConnectionMeasurement(at: Date(timeIntervalSince1970: 1_760_000_000), mac: "a1b2c3", operation: "perform",
            method: "GET", kind: kind, route: route, ms: ms, network: "cellular", radio: radio, outcome: outcome)
    }

    private func make(_ answers: [Result<APIResponse, any Error>],
                      route: ConnectionRoute) -> (MonitoredOperations, Observed) {
        let observed = Observed()
        let monitor = ConnectionMonitor { _ in .macNotAnswering }
        let operations = MonitoredOperations(ScriptedAnswers(answers), monitor: monitor, route: { route },
            record: { observation in observed.withLock { $0.append(observation) } },
            stopwatch: { { .milliseconds(120) } })
        return (operations, observed)
    }
}

private actor ScriptedAnswers: OperationAPI {
    private var answers: [Result<APIResponse, any Error>]
    init(_ answers: [Result<APIResponse, any Error>]) { self.answers = answers }

    func perform(_ request: APIRequest) async throws -> APIResponse { try answers.removeFirst().get() }
    func recover(_ request: APIRequest) async throws -> APIResponse { try answers.removeFirst().get() }
}

private final class Observed: Sendable {
    private let items = Mutex<[RequestObservation]>([])
    func withLock<T: Sendable>(_ body: (inout [RequestObservation]) -> T) -> T { items.withLock { body(&$0) } }

    @Test func theTimeToOpenTheConnectionIsAGroupOfItsOwn() {
        let network = ConnectionMeasurement.NetworkSnapshot(interface: "cellular", radio: "NR")
        let connect = ConnectionMeasurement.connected(after: .milliseconds(740), route: .direct, at: Date(), mac: "a1b2c3",
                                                      network: network)
        #expect(connect.ms == 740)
        #expect(connect.kind == ConnectionMeasurement.connectKind)
        let request = ConnectionMeasurement(at: Date(), mac: "a1b2c3", operation: "perform", method: "GET", kind: "views",
                                            route: .direct, ms: 55, network: "cellular", radio: "NR", outcome: .ok)
        let summary = ConnectionMeasurementSummary.groups([connect, request])
        #expect(summary.map(\.group) == [.requests, .connecting])
        #expect(summary.last?.medianMs == 740)
    }
}
