import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI
import XCTest

/// Settings › Metrics as the daemon serves it to the iPhone: a week against the week before, with
/// three models and two projects; and the moment after choosing another period, the choice marked
/// and the figures dimmed until the daemon answers.
@MainActor
final class MetricsSnapshotTests: XCTestCase {
    private func metrics(_ document: ViewDocument, key: String? = nil, chosen: String? = nil) -> some View {
        var state = MetricsFeature.State(key: ViewKey(rawValue: key ?? document.key))
        state.document = document
        state.offered = true
        state.chosen = chosen
        let store = Store(initialState: state) { EmptyReducer<MetricsFeature.State, MetricsFeature.Action>() }
        return NavigationStack { MetricsScreen(store: store) }
    }

    func test_metrics() throws {
        let document = try SampleViews.document("metrics-ios")
        assertScreen([.light, .dark]) { ScreenFrame(height: 2900) { metrics(document) } }
    }

    func test_metrics_spanish() throws {
        let document = try SampleViews.document("metrics-ios-es")
        assertScreen([.light, .dark], language: .spanish) { ScreenFrame(height: 2900) { metrics(document) } }
    }

    func test_metrics_changingPeriod() throws {
        let document = try SampleViews.document("metrics-ios")
        assertScreen([.light]) {
            ScreenFrame(height: 1100) { metrics(document, key: "metrics?period=30d&compare=previous", chosen: "period:30d") }
        }
    }
}
