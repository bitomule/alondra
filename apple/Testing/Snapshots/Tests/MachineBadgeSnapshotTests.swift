import AlondraContract
import AlondraFeatures
import AlondraiOSUI
import ComposableArchitecture
import SnapshotTesting
import SwiftUI
import XCTest

/// The Mac pill as the navigation bar draws it, next to Settings as the app hosts Waiting for You.
/// A pill wider than its slot is folded with Settings into a "…" menu, which only a real window
/// shows, so these tests draw one instead of laying the view out on its own.
@MainActor
final class MachineBadgeSnapshotTests: XCTestCase {
    override func setUp() async throws { ScreenStores.pinTime() }

    private struct Harness: View {
        let store: StoreOf<TriageFeature>
        @Namespace private var zoom

        var body: some View {
            NavigationStack {
                TriageView(store: store, zoom: zoom)
                    .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Settings", systemImage: "gearshape") {} } }
            }
        }
    }

    private static let status = #"{"version":"v1","summary":"0 nodos trabajando","badge":0,"counts":[],"gauges":[{"id":"claude_week","label":"Claude semana","value":0.83,"text":"83%","detail":"El límite semanal se reinicia","level":"calm"},{"id":"claude_today","label":"Claude hoy","value":0.0,"text":"0/6%","detail":"Gastado 0% de la cuota diaria del 6%","level":"calm"},{"id":"claude_five_hours","label":"Claude 5 h","value":0.0,"text":"0%","detail":"El límite de 5 horas se reinicia","level":"calm"},{"id":"memory","label":"RAM","value":0.39,"text":"39%","level":"calm"},{"id":"cpu","label":"CPU","value":0.24,"text":"24%","level":"calm"},{"id":"disk","label":"Disco","value":0.88,"text":"58 GB libres","level":"warm"}],"orchestrator":{"text":"inactivo","tone":"neutral"}}"#

    private func navigationBar(width: CGFloat) throws -> UIImage {
        let status = try ContractCodec.decode(ViewStatus.self, from: Data(Self.status.utf8))
        let store = try ScreenStores.triage(queue: []) { $0.status = status }
        let scene = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(x: 0, y: 0, width: width, height: 874)
        window.overrideUserInterfaceStyle = .light
        window.rootViewController = UIHostingController(rootView: Harness(store: store))
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        RunLoop.main.run(until: Date().addingTimeInterval(1))
        let bar = CGRect(x: 0, y: 0, width: width, height: 120)
        return UIGraphicsImageRenderer(bounds: bar).image { _ in
            window.drawHierarchy(in: window.bounds, afterScreenUpdates: true)
        }
    }

    func test_proMax_showsEveryFigure() throws {
        assertAppSnapshot(of: try navigationBar(width: 440), as: .image(precision: 0.99, perceptualPrecision: 0.97))
    }

    func test_pro_keepsTheClaudeFiguresAndSettings() throws {
        assertAppSnapshot(of: try navigationBar(width: 402), as: .image(precision: 0.99, perceptualPrecision: 0.97))
    }

    func test_narrow_keepsEveryBarAndSettings() throws {
        assertAppSnapshot(of: try navigationBar(width: 375), as: .image(precision: 0.99, perceptualPrecision: 0.97))
    }
}
