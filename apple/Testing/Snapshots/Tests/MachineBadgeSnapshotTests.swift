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
        var connection = ConnectionState.connected(.irohRelay)
        @Namespace private var zoom

        var body: some View {
            NavigationStack {
                TriageView(store: store, zoom: zoom)
                    .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Settings", systemImage: "gearshape") {} } }
            }
            .environment(\.connectionState, connection)
        }
    }

    private static let status = #"{"version":"v1","summary":"0 nodos trabajando","badge":0,"counts":[],"gauges":[{"id":"claude_week","label":"Claude semana","value":0.83,"text":"83%","detail":"El límite semanal se reinicia","level":"calm"},{"id":"claude_today","label":"Claude hoy","value":0.0,"text":"0/6%","detail":"Gastado 0% de la cuota diaria del 6%","level":"calm"},{"id":"claude_five_hours","label":"Claude 5 h","value":0.0,"text":"0%","detail":"El límite de 5 horas se reinicia","level":"calm"},{"id":"memory","label":"RAM","value":0.39,"text":"39%","level":"calm"},{"id":"cpu","label":"CPU","value":0.24,"text":"24%","level":"calm"},{"id":"disk","label":"Disco","value":0.88,"text":"58 GB libres","level":"warm"}],"orchestrator":{"text":"inactivo","tone":"neutral"}}"#

    private static let fullDiskStatus = status
        .replacingOccurrences(of: #""value":0.88,"text":"58 GB libres","level":"warm""#, with: #""value":0.96,"text":"16 GB libres","level":"hot""#)

    private func navigationBar(width: CGFloat, status raw: String = status,
                               connection: ConnectionState = .connected(.irohRelay)) throws -> UIImage {
        let home = try Fixtures.document("home-ios") { object in
            object["status"] = try JSONSerialization.jsonObject(with: Data(raw.utf8))
            var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
            sections[0]["card_ids"] = [String]()
            object["sections"] = sections
        }
        let store = try ScreenStores.triage(queue: [], home: home)
        let scene = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(x: 0, y: 0, width: width, height: 874)
        window.overrideUserInterfaceStyle = .light
        window.rootViewController = UIHostingController(rootView: Harness(store: store, connection: connection))
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        RunLoop.main.run(until: Date().addingTimeInterval(1))
        let bar = CGRect(x: 0, y: 0, width: width, height: 120)
        return UIGraphicsImageRenderer(bounds: bar).image { _ in
            window.drawHierarchy(in: window.bounds, afterScreenUpdates: true)
        }
    }

    func test_proMax_showsEveryFigure() throws {
        assertAppSnapshot(of: try navigationBar(width: 440), as: Snapshotting<UIImage, UIImage>.image(precision: 0.99, perceptualPrecision: 0.97).asRecorded)
    }

    func test_pro_keepsTheClaudeFiguresAndSettings() throws {
        assertAppSnapshot(of: try navigationBar(width: 402), as: Snapshotting<UIImage, UIImage>.image(precision: 0.99, perceptualPrecision: 0.97).asRecorded)
    }

    func test_narrow_keepsEveryBarAndSettings() throws {
        assertAppSnapshot(of: try navigationBar(width: 375), as: Snapshotting<UIImage, UIImage>.image(precision: 0.99, perceptualPrecision: 0.97).asRecorded)
    }

    func test_fullDisk_takesOverTheMachineReading() throws {
        assertAppSnapshot(of: try navigationBar(width: 440, status: Self.fullDiskStatus), as: Snapshotting<UIImage, UIImage>.image(precision: 0.99, perceptualPrecision: 0.97).asRecorded)
    }

    func test_connecting_dimsTheFiguresBehindAGreyDot() throws {
        assertAppSnapshot(of: try navigationBar(width: 440, connection: .connecting),
                          as: Snapshotting<UIImage, UIImage>.image(precision: 0.99, perceptualPrecision: 0.97).asRecorded)
    }

    func test_macNotAnswering_dimsTheFiguresBehindARedDot() throws {
        assertAppSnapshot(of: try navigationBar(width: 440, connection: .unreachable(.macNotAnswering)),
                          as: Snapshotting<UIImage, UIImage>.image(precision: 0.99, perceptualPrecision: 0.97).asRecorded)
    }

    func test_popover_breaksTheMachineDownIntoRamAndCpu() throws {
        let status = try ContractCodec.decode(ViewStatus.self, from: Data(Self.status.utf8))
        assertScreen([.light]) { ScreenFrame(height: 260) { MachineDetail(gauges: status.gauges) } }
    }
}
