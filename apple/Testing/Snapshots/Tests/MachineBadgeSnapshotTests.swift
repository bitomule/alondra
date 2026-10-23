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

    private static let status = #"{"version":"v1","summary":"0 nodos trabajando","badge":0,"counts":[],"gauges":[{"id":"claude_margin","label":"Claude · margen","value":0.35,"text":"35%","detail":"Lo que queda de la semana, de la parte de hoy y de la ventana de 5 h; el trabajo solo espera cuando todas las cuentas llegan a su límite · la semana se reinicia el lunes a las 02:00","level":"calm","parts":[{"label":"Principal","value":0.7,"text":"70%","detail":"semana 30% · 5 h 10% · hoy 2/15%","level":"calm"},{"label":"Trabajo","value":0.0,"text":"0%","detail":"semana 60% · 5 h 95% · hoy 4/8%","level":"warm"},{"label":"API","value":0.0,"text":"por uso","detail":"De pago por uso: solo la usan las peticiones que la eligen","level":"calm"}]},{"id":"claude_five_hours","label":"Claude 5 h","value":0.0,"text":"0%","detail":"El límite de 5 horas se reinicia","level":"calm"},{"id":"memory","label":"RAM","value":0.39,"text":"39%","level":"calm"},{"id":"cpu","label":"CPU","value":0.24,"text":"24%","level":"calm"},{"id":"disk","label":"Disco","value":0.88,"text":"58 GB libres","level":"warm"}],"orchestrator":{"text":"inactivo","tone":"neutral"}}"#

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

    private static let englishMargin = #"[{"id":"claude_margin","label":"Claude · margin","value":0.0,"text":"0%","detail":"Every Claude account is at its limit: new work waits","level":"hot","parts":[{"label":"Principal","value":0.0,"text":"limit","detail":"At its limit; back in 2 h (14:30) · week 100% · 5 h 40%","level":"hot"},{"label":"Trabajo","value":0.0,"text":"signed out","detail":"Sign in on the Mac: alondra accounts login \"Trabajo\"","level":"hot"}]},{"id":"claude_five_hours","label":"Claude 5 h","value":0.4,"text":"40%","detail":"5-hour limit resets in 2 h (14:30)","level":"calm"}]"#

    func test_popover_breaksTheMachineDownIntoRamAndCpu() throws {
        let status = try ContractCodec.decode(ViewStatus.self, from: Data(Self.status.utf8))
        assertScreen([.light, .dark], language: .spanish) { ScreenFrame(height: 480) { MachineDetail(gauges: status.gauges) } }
    }

    func test_popover_listsEveryClaudeAccountWhenNoneCanWork() throws {
        let gauges = try ContractCodec.decode([AlondraContract.Gauge].self, from: Data(Self.englishMargin.utf8))
        assertScreen([.light, .dark]) { ScreenFrame(height: 300) { MachineDetail(gauges: gauges) } }
    }
}
