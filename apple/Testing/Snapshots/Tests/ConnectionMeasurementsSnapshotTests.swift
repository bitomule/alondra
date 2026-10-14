import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import SnapshotTesting
import SwiftUI
import XCTest

/// Settings' connection measurements on the iPhone: each path and network with its times.
@MainActor
final class ConnectionMeasurementsSnapshotTests: XCTestCase {
    static let summary = [
        ConnectionMeasurementSummary(route: .relay, network: "cellular", waits: false, count: 184, failures: 3, medianMs: 352, p90Ms: 611),
        ConnectionMeasurementSummary(route: .localNetwork, network: "wifi", waits: false, count: 92, failures: 0, medianMs: 14, p90Ms: 38),
        ConnectionMeasurementSummary(route: nil, network: "cellular", waits: false, count: 1, failures: 1, medianMs: nil, p90Ms: nil),
        ConnectionMeasurementSummary(route: .relay, network: "cellular", waits: true, count: 40, failures: 2, medianMs: 24_870, p90Ms: 50_012),
    ]

    private func section(_ summary: [ConnectionMeasurementSummary]) -> some View {
        var state = ConnectionMeasurementsFeature.State()
        state.summary = summary
        state.file = URL(fileURLWithPath: "/tmp/connection-measurements.jsonl")
        let store = Store(initialState: state) {
            EmptyReducer<ConnectionMeasurementsFeature.State, ConnectionMeasurementsFeature.Action>()
        }
        return Form { ConnectionMeasurementsSection(store: store) }
            .scrollContentBackground(.hidden)
            .background(DawnBackground())
    }

    func test_connectionMeasurements() {
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 560) { section(Self.summary) } }
    }

    func test_connectionMeasurements_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 560) { section(Self.summary) } }
    }

    func test_connectionMeasurements_empty() {
        assertScreen([.light], inWindow: true) { ScreenFrame(height: 300) { section([]) } }
    }
}
