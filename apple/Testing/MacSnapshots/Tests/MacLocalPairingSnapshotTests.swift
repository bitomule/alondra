import AlondraContract
import AlondraFeatures
import AlondraFeed
import AlondraMacWorkspace
import AppKit
import ComposableArchitecture
import SnapshotTesting
import SwiftUI
import XCTest

/// What this Mac shows about other devices: the request to allow one, and the list of the
/// devices and Macs it works with, beside how many nodes work at once.
@MainActor
final class MacLocalPairingSnapshotTests: XCTestCase {
    override func setUp() async throws {
        _ = NSApplication.shared
    }

    private func assertWindow<Content: View>(
        size: CGSize, spanish: Bool = false, variants: [(String, NSAppearance.Name)] = [("light", .aqua), ("dark", .darkAqua)],
        file: StaticString = #filePath, testName: String = #function, line: UInt = #line,
        @ViewBuilder _ content: () -> Content
    ) {
        let view = AnyView(content().environment(\.locale, spanish ? Locale(identifier: "es_ES") : .current))
        for (name, appearance) in variants {
            let hosting = NSHostingView(rootView: view.background(Color(nsColor: .windowBackgroundColor)))
            let window = snapshotWindow(size: size)
            defer { window.close() }
            window.appearance = NSAppearance(named: appearance)
            window.contentView = hosting
            hosting.frame = CGRect(origin: .zero, size: size)
            hosting.layoutSubtreeIfNeeded()
            assertAppSnapshot(of: hosting as NSView, as: .image(perceptualPrecision: 0.98, size: size),
                              named: spanish ? "\(name)_es" : name, file: file, testName: testName, line: line)
        }
    }

    private let light: [(String, NSAppearance.Name)] = [("light", .aqua)]

    func test_pairingRequest_asksWithTheCode() {
        assertWindow(size: CGSize(width: 440, height: 300)) { pairingRequest }
    }

    func test_pairingRequest_asksWithTheCode_spanish() {
        assertWindow(size: CGSize(width: 440, height: 300), spanish: true, variants: light) { pairingRequest }
    }

    private var pairingRequest: some View {
        MacPairingRequestSheet(request: PairingRequest(id: "p-1", name: "iPhone de David", code: "4F2A91"),
                               error: nil, decide: { _ in })
    }

    func test_settings_thisMacItsPairedMacsAndItsDevices() throws {
        let document = try machine("machine-settings-mac")
        let mcpServers = try machine("mcp-servers-mac")
        assertWindow(size: CGSize(width: 560, height: 1420)) { settings(thisMac: "This Mac", machine: document, mcpServers: mcpServers) }
    }

    func test_settings_thisMacItsPairedMacsAndItsDevices_spanish() throws {
        let document = try machine("machine-settings-ios-es")
        let mcpServers = try machine("mcp-servers-ios-es")
        assertWindow(size: CGSize(width: 560, height: 1500), spanish: true, variants: light) { settings(thisMac: "Este Mac", machine: document, mcpServers: mcpServers) }
    }

    private func machine(_ name: String) throws -> ViewDocument {
        let environment = ProcessInfo.processInfo.environment
        let url = URL(fileURLWithPath: try XCTUnwrap(environment["TEST_SRCDIR"]))
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views/cards/\(name).json")
        return try ContractCodec.decode(ViewDocument.self, from: Data(contentsOf: url))
    }

    private func settings(thisMac: String, machine: ViewDocument, mcpServers: ViewDocument) -> some View {
        var state = SettingsFeature.State(service: "127.0.0.1", device: "mac", supportsNotifications: false, usesAccount: false)
        let ids = (1...3).map { UUID(uuidString: "00000000-0000-0000-0000-00000000000\($0)")! }
        state.macs = [
            MacChoice(id: ids[0], macID: "this-mac", name: thisMac),
            MacChoice(id: ids[1], macID: "mac-b27f03d9", name: "Mac mini", local: true),
            MacChoice(id: ids[2], macID: "mac-c55e10aa", name: "MacBook Pro", local: true),
        ]
        state.selectedMacID = ids[0]
        state.showsSignOutWithMacs = true
        state.machineSettings.settings = MachineSetting.all(in: machine)
        state.allowedRules.rules = AllowedRule.all(in: machine)
        state.mcpServers.document = mcpServers
        state.macStatuses = [ids[0]: .route(.thisMac), ids[1]: .route(.localNetwork), ids[2]: .problem(.macNotOnNetwork)]
        state.devices = [
            TrustedDevice(id: "phone-1", name: "iPhone", origin: .direct, lastUsed: Date(timeIntervalSinceNow: -300)),
            TrustedDevice(id: "phone-2", name: "iPad", origin: .relay),
        ]
        let store = Store(initialState: state) { EmptyReducer<SettingsFeature.State, SettingsFeature.Action>() }
        return NavigationStack { MacSettingsView(store: store) { _ in EmptyView() } }
    }

    private var thisMacSettings: some View {
        let client = MacSettingsClient(load: { throw CancellationError() }, save: { _ in throw CancellationError() },
                                       folders: { _ in throw CancellationError() })
        let state = MacSettingsFeature.State(id: UUID(), macName: "MacBook Pro de David", client: client, settings: MacSettings(
            projectsRoot: "/Users/david/Projects",
            candidates: [.init(path: "/Users/david/Projects", repos: 63), .init(path: "/Users/david/Developer", repos: 4)],
            relay: .init(on: false)))
        let store = Store(initialState: state) { EmptyReducer<MacSettingsFeature.State, MacSettingsFeature.Action>() }
        return NavigationStack { ThisMacSettingsView(store: store) }
    }

    private var connectionMeasurements: some View {
        var state = ConnectionMeasurementsFeature.State()
        state.summary = [
            ConnectionMeasurementSummary(route: .thisMac, network: "wifi", count: 512, failures: 0, medianMs: 4, p90Ms: 11),
            ConnectionMeasurementSummary(route: .direct, network: "wired", count: 88, failures: 0, medianMs: 9, p90Ms: 21),
            ConnectionMeasurementSummary(route: .irohRelay, network: "wired", count: 37, failures: 2, medianMs: 121, p90Ms: 240),
            ConnectionMeasurementSummary(route: .direct, network: "wired", group: .connecting, count: 5, failures: 0, medianMs: 310, p90Ms: 620),
            ConnectionMeasurementSummary(route: .thisMac, network: "wifi", group: .liveUpdates, count: 64, failures: 0, medianMs: 49_880, p90Ms: 50_004),
        ]
        state.file = URL(fileURLWithPath: "/tmp/connection-measurements.jsonl")
        let store = Store(initialState: state) {
            EmptyReducer<ConnectionMeasurementsFeature.State, ConnectionMeasurementsFeature.Action>()
        }
        return Form { ConnectionMeasurementsSection(store: store) }.formStyle(.grouped)
    }

    private func crashReport(_ state: CrashReportFeature.State) -> some View {
        let store = Store(initialState: state) { EmptyReducer<CrashReportFeature.State, CrashReportFeature.Action>() }
        return NavigationStack { CrashReportScreen(store: store) }
    }

    private var crashReportPreview: CrashReportFeature.State {
        let report = CrashReport(
            id: "r-20261027-0000abcd", compressedBytes: 6_144, repo: "bitomule/alondra-issues", passed: true,
            files: [.init(name: "manifest.json", bytes: nil), .init(name: "description.txt", bytes: 31),
                    .init(name: "versions.json", bytes: 412), .init(name: "machine.json", bytes: 1_204),
                    .init(name: "daemon.log", bytes: 48_310), .init(name: "lifecycle.jsonl", bytes: 9_822),
                    .init(name: "nodes.jsonl", bytes: nil, empty: "no nodes in the window"), .init(name: "apps.jsonl", bytes: 7_480)])
        return CrashReportFeature.State(whatHappened: "The node stopped after the Mac slept", stage: .ready(report))
    }

    func test_crashReport_preview() {
        assertWindow(size: CGSize(width: 560, height: 1000)) { crashReport(crashReportPreview) }
    }

    func test_crashReport_preview_spanish() {
        assertWindow(size: CGSize(width: 560, height: 1000), spanish: true, variants: light) { crashReport(crashReportPreview) }
    }

    func test_crashReport_whatHappened() {
        assertWindow(size: CGSize(width: 560, height: 360)) { crashReport(.init()) }
    }

    func test_crashReport_whatHappened_spanish() {
        assertWindow(size: CGSize(width: 560, height: 360), spanish: true, variants: light) { crashReport(.init()) }
    }

    func test_crashReport_serviceNotAnswering() {
        assertWindow(size: CGSize(width: 560, height: 240)) { crashReport(.init(stage: .failed(.notAnswering(onThisMac: true), report: nil))) }
    }

    func test_crashReport_serviceNotAnswering_spanish() {
        assertWindow(size: CGSize(width: 560, height: 240), spanish: true, variants: light) {
            crashReport(.init(stage: .failed(.notAnswering(onThisMac: true), report: nil)))
        }
    }

    func test_connectionMeasurements() {
        assertWindow(size: CGSize(width: 560, height: 480)) { connectionMeasurements }
    }

    func test_connectionMeasurements_spanish() {
        assertWindow(size: CGSize(width: 560, height: 480), spanish: true, variants: light) { connectionMeasurements }
    }

    func test_thisMacSettings() {
        assertWindow(size: CGSize(width: 480, height: 460)) { thisMacSettings }
    }

    func test_thisMacSettings_spanish() {
        assertWindow(size: CGSize(width: 480, height: 460), spanish: true, variants: light) { thisMacSettings }
    }

    private var directConnection: some View {
        let direct = DirectConnection(
            verdict: .relayOnly, reasons: [.relayOnSameNetwork, .routerOffersNoMapping, .noIpv6], portMapping: .init(),
            nat: .unknown, port: 47811, lanIp: "192.168.1.20", sessions: .init(lan: 6, internet: 0, relay: 2))
        return Form { DirectConnectionSection(direct: direct) }.formStyle(.grouped)
    }

    func test_directConnection_relayOnlyWithTheRouterGuide() {
        assertWindow(size: CGSize(width: 560, height: 760)) { directConnection }
    }

    func test_directConnection_relayOnlyWithTheRouterGuide_spanish() {
        assertWindow(size: CGSize(width: 560, height: 760), spanish: true, variants: light) { directConnection }
    }

    func test_localDaemonMissing_installWithHomebrew() {
        assertWindow(size: CGSize(width: 640, height: 480)) { MacLocalDaemonMissingView(connectToAnotherMac: {}, exploreDemo: {}) }
    }

    func test_localDaemonMissing_installWithHomebrew_spanish() {
        assertWindow(size: CGSize(width: 640, height: 480), spanish: true, variants: light) {
            MacLocalDaemonMissingView(connectToAnotherMac: {}, exploreDemo: {})
        }
    }
}
