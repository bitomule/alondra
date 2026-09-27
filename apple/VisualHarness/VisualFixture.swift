import AlondraCloudPairingFeature
import AlondraDemoWorkspace
import AlondraDomain
import AlondraSettingsFeature
import AlondraShared
import AlondraTriageFeature
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Observation

/// Compiled only into the visual test hosts. There is no production owner,
/// URLSession, Keychain, notification delegate, or configurable service endpoint.
@MainActor @Observable
final class VisualFixture {
    nonisolated static let scenarios = ["reconnect", "account-settings", "mac-selection"]
    nonisolated static let alpha = UUID(uuidString: "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA")!
    nonisolated static let beta = UUID(uuidString: "BBBBBBBB-BBBB-BBBB-BBBB-BBBBBBBBBBBB")!
    let runID: String
    let scenario: String
    let directory: URL
    private(set) var ready = false
    private(set) var workspace: StoreOf<ServiceRootFeature>!
    private(set) var selected = VisualFixture.alpha
    private var selectedByUI = false
    private var lateDelivered = false
    private var sawRestoring = false
    private var processedControl = 0
    private let demo: DemoWorkspace
    private let feed: VisualFeed
    private let restoration: AsyncStream<Void>
    private let releaseRestoration: AsyncStream<Void>.Continuation
    @ObservationIgnored private var monitorStarted = false

    @ObservationIgnored lazy var pairing = Store(initialState: CloudPairingFeature.State()) {
        CloudPairingFeature()
    } withDependencies: {
        var client = CloudPairingService(restore: { [weak self] in
            guard let self else { throw CancellationError() }
            return try await self.restore()
        }, login: { throw CancellationError() }, begin: { _ in throw CancellationError() },
        approve: { throw CancellationError() }, cancel: {}, logout: { throw CancellationError() })
        client.hasSavedConfiguration = { true }
        $0.cloudPairing = client
    }
    @ObservationIgnored lazy var settings = makeSettings()

    private func makeSettings() -> StoreOf<ServiceSettingsFeature> {
        var state = ServiceSettingsFeature.State(service: "synthetic.invalid", device: "visual-client", supportsNotifications: false)
        state.macs = [ServiceMacChoice(id: Self.alpha, macID: "alpha-mac"), ServiceMacChoice(id: Self.beta, macID: "beta-mac")]
        state.selectedMacID = selected
        var client = ServiceSettingsClient(observe: { AsyncStream { $0.finish() } },
            save: { _, _ in throw CancellationError() }, requestPermission: {}, openSystemSettings: {},
            checkConnection: {}, logout: {})
        client.selectMac = { [weak self] id in
            guard let self else { throw CancellationError() }
            try await self.select(id)
        }
        return Store(initialState: state) { ServiceSettingsFeature() } withDependencies: {
            $0.serviceSettings = client
            $0.cloudPairing = CloudPairingService(restore: { false }, login: { throw CancellationError() },
                begin: { _ in throw CancellationError() }, approve: { throw CancellationError() }, cancel: {}, logout: {})
        }
    }

    init(environment: [String: String]) throws {
        guard let run = environment["ALONDRA_VISUAL_RUN_ID"], UUID(uuidString: run) != nil,
              let scenario = environment["ALONDRA_VISUAL_SCENARIO"], Self.scenarios.contains(scenario) else {
            throw FixtureError.invalidLaunch
        }
        runID = run; self.scenario = scenario
        #if os(iOS)
        directory = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        #else
        guard let path = environment["ALONDRA_VISUAL_DIRECTORY"], path.hasPrefix("/") else { throw FixtureError.invalidLaunch }
        directory = URL(fileURLWithPath: path, isDirectory: true)
        #endif
        let pending = AsyncStream<Void>.makeStream()
        restoration = pending.stream; releaseRestoration = pending.continuation
        demo = try DemoWorkspace(now: Date(timeIntervalSince1970: 1_700_000_000))
        feed = try VisualFeed()
        workspace = makeWorkspace(identity: Self.alpha, settingsPresented: scenario != "reconnect")
    }

    private func makeWorkspace(identity: UUID, settingsPresented: Bool) -> StoreOf<ServiceRootFeature> {
        var initial = ServiceRootFeature.State()
        initial.settingsPresented = settingsPresented
        let demo = demo, feed = feed
        let store = Store(initialState: initial) { ServiceRootFeature() } withDependencies: {
            demo.install(into: &$0)
            $0.triageService = TriageService(observe: { await feed.triage() }, mutate: { _, _, _ in .obsolete })
            $0.serviceTasksClient = ServiceTasksClient(refresh: {}, observe: { await feed.snapshots() })
        }
        // This is the same public identity action used by the production feature.
        // Delayed fixture updates are NOT filtered before reaching its reducer.
        store.send(.triage(.triage(.identityChanged(VisualFeed.identity(identity)))))
        return store
    }

    private func restore() async throws -> Bool {
        if scenario == "reconnect" {
            for await _ in restoration { break }
            try Task.checkCancellation()
        }
        ready = true
        return true
    }

    private func select(_ id: UUID) async throws {
        guard scenario == "mac-selection", id == Self.beta, selected == Self.alpha else { throw FixtureError.unexpectedAction }
        selectedByUI = true
        selected = id
        workspace.send(.disappeared)
        workspace.send(.tasks(.disappeared)); workspace.send(.fleet(.disappeared))
        try await feed.selectBeta()
        settings = makeSettings()
        workspace = makeWorkspace(identity: id, settingsPresented: false)
    }

    func monitor() async {
        guard !monitorStarted else { return }
        monitorStarted = true
        let deadline = ContinuousClock.now.advanced(by: .seconds(150))
        defer {
            releaseRestoration.finish()
            workspace.send(.disappeared)
            workspace.send(.tasks(.disappeared)); workspace.send(.fleet(.disappeared))
            settings.send(.disappeared)
            Task { await feed.close(); await demo.close() }
        }
        do {
            while !Task.isCancelled, ContinuousClock.now < deadline {
                if pairing.configuration == .saved && pairing.restorationInFlight && !ready { sawRestoring = true }
                var finish = false
                let control = directory.appendingPathComponent("visual-control.json")
                if let data = try? Data(contentsOf: control) {
                    let command = try JSONDecoder().decode(Control.self, from: data)
                    guard command.runID == runID else { throw FixtureError.invalidControl }
                    if command.sequence > processedControl {
                        processedControl = command.sequence
                        switch command.action {
                        case "restore":
                            guard scenario == "reconnect", sawRestoring else { throw FixtureError.unexpectedAction }
                            releaseRestoration.yield(()); releaseRestoration.finish()
                        case "late-alpha":
                            guard selectedByUI, selected == Self.beta, workspace.triage.triage.hasSnapshot else { throw FixtureError.unexpectedAction }
                            // Await the actual reducer processing, not a timed guess.
                            await workspace.send(.triage(.triage(.inboxUpdated(try VisualFeed.update(Self.alpha))))).finish()
                            lateDelivered = true
                        case "finish": finish = true
                        default: throw FixtureError.invalidControl
                        }
                    }
                }
                let checks = checks()
                try write(status: finish ? (checks.values.allSatisfy { $0 } ? "passed" : "failed") : "running", checks: checks)
                if finish { return }
                try await Task.sleep(for: .milliseconds(40))
            }
            try write(status: "failed", checks: checks(), error: "timeout_or_cancelled")
        } catch {
            try? write(status: "failed", checks: checks(), error: "fixture_failed")
        }
    }

    private func checks() -> [String: Bool] {
        let alpha = workspace.triage.triage.items.contains { $0.id == "q101" }
        let beta = workspace.triage.triage.items.contains { $0.id == "q202" }
        switch scenario {
        case "reconnect":
            return ["saved_restoration_observed": sawRestoring,
                    "workspace_remained_selected": pairing.showsWorkspace,
                    "restoration_completed": ready && pairing.phase == .approved,
                    "alpha_content_loaded": alpha && workspace.triage.triage.hasSnapshot]
        case "account-settings":
            return ["settings_sheet_presented": workspace.settingsPresented,
                    "real_settings_account": settings.service == "synthetic.invalid" && settings.macs.count == 2,
                    "alpha_selected": settings.selectedMacID == Self.alpha,
                    "workspace_approved": ready && pairing.phase == .approved]
        default:
            return ["selection_requested_by_settings": selectedByUI,
                    "beta_selected": selected == Self.beta && settings.selectedMacID == Self.beta,
                    "beta_content_loaded": beta && workspace.triage.triage.hasSnapshot,
                    "late_alpha_delivered": lateDelivered,
                    "late_alpha_rejected": lateDelivered && !alpha && beta && workspace.triage.triage.identity == VisualFeed.identity(Self.beta)]
        }
    }

    private func write(status: String, checks: [String: Bool], error: String? = nil) throws {
        var value: [String: Any] = ["schema": 1, "scope": "visual-native-fixture", "runID": runID,
            "scenario": scenario, "status": status, "checks": checks, "controlSequence": processedControl,
            "ready": ready, "restoring": pairing.restorationInFlight, "savedConfiguration": pairing.configuration == .saved,
            "settingsPresented": workspace.settingsPresented, "selectedMac": selected == Self.alpha ? "alpha" : "beta",
            "inboxIDs": workspace.triage.triage.items.map(\.id)]
        if let error { value["error"] = error }
        try JSONSerialization.data(withJSONObject: value, options: [.prettyPrinted, .sortedKeys])
            .write(to: directory.appendingPathComponent("visual-result.json"), options: .atomic)
    }
    private struct Control: Decodable { let runID: String; let sequence: Int; let action: String }
    private enum FixtureError: Error { case invalidLaunch, invalidControl, unexpectedAction }
}

private actor VisualFeed {
    private var current: TriageInboxUpdate
    private var triageObservers: [UUID: AsyncStream<TriageInboxUpdate>.Continuation] = [:]
    private var snapshotsObservers: [UUID: AsyncStream<Overview?>.Continuation] = [:]
    init() throws { current = try Self.update(VisualFixture.alpha) }
    nonisolated static func identity(_ id: UUID) -> TriageIdentity {
        TriageIdentity(server: URL(string: "https://synthetic.invalid")!, deviceID: id.uuidString,
            credentialRevision: "visual-fixture", generation: id == VisualFixture.alpha ? 1 : 2)
    }
    nonisolated static func update(_ id: UUID) throws -> TriageInboxUpdate {
        let alpha = id == VisualFixture.alpha
        let value: [String: Any] = ["tick": alpha ? 1 : 2, "nodes": [], "news": [], "orchestratorMessages": [],
            "machine": ["memoryTotal": 16_000_000_000, "memoryAvailable": 10_000_000_000, "load": [0.2, 0.3, 0.2], "diskAvailable": 120_000_000_000, "unreliable": false],
            "openQuestions": [["id": alpha ? 101 : 202, "sessionId": alpha ? "alpha-session" : "beta-session",
                "nodeName": alpha ? "Alpha assistant" : "Beta assistant", "title": alpha ? "Alpha Mac question" : "Beta Mac question",
                "detail": "Synthetic visual fixture. No real account or agent.", "options": [["label": "Keep current"]],
                "links": [], "attachments": [], "origin": "ask", "status": "open", "askedAt": 1_700_000_000_000]],
            "prs": [], "reports": [], "tasks": [], "requests": [], "user": "Visual fixture"]
        let overview = try JSONDecoder().decode(Overview.self, from: JSONSerialization.data(withJSONObject: value))
        return TriageInboxUpdate(identity: identity(id), overview: overview, connection: .online)
    }
    func triage() -> AsyncStream<TriageInboxUpdate> {
        let id = UUID(), pair = AsyncStream<TriageInboxUpdate>.makeStream(bufferingPolicy: .bufferingNewest(1))
        triageObservers[id] = pair.continuation; pair.continuation.yield(current)
        pair.continuation.onTermination = { [weak self] _ in Task { await self?.remove(id) } }
        return pair.stream
    }
    func snapshots() -> AsyncStream<Overview?> {
        let id = UUID(), pair = AsyncStream<Overview?>.makeStream(bufferingPolicy: .bufferingNewest(1))
        snapshotsObservers[id] = pair.continuation; pair.continuation.yield(current.overview)
        pair.continuation.onTermination = { [weak self] _ in Task { await self?.remove(id) } }
        return pair.stream
    }
    func selectBeta() throws { current = try Self.update(VisualFixture.beta) }
    func remove(_ id: UUID) { triageObservers.removeValue(forKey: id); snapshotsObservers.removeValue(forKey: id) }
    func close() {
        for value in triageObservers.values { value.finish() }
        for value in snapshotsObservers.values { value.finish() }
        triageObservers.removeAll(); snapshotsObservers.removeAll()
    }
}
