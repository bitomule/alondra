import AlondraAccount
import AlondraFeatures
import ComposableArchitecture
import Observation
import SwiftUI

struct MacServiceSettingsScene: View {
    let services: Connection
    let owner: AccountSession?
    let workspace: MacWorkspaceOwner
    @State private var context = MacSettingsContext()
    var body: some View {
        Group {
            if let store = context.store { MacSettingsView(store: store) { pairing in
                MacServicePairingScreen(store: pairing).frame(minWidth: 600, minHeight: 600)
            } }
            else { ProgressView("Loading settings…") }
        }.task { await context.connect(services: services, owner: owner, workspace: workspace) }
    }
}

@MainActor @Observable private final class MacSettingsContext {
    private(set) var store: StoreOf<SettingsFeature>?
    func connect(services: Connection, owner: AccountSession?, workspace: MacWorkspaceOwner) async {
        guard store == nil else { return }
        let api = await services.api
        let account = workspace.account
        var client = SettingsClient(observe: { AsyncStream { $0.finish() } },
            save: { _, _ in throw CancellationError() }, requestPermission: {}, openSystemSettings: {},
            checkConnection: { try await services.checkSettingsConnection() },
            logout: { await MainActor.run {
                guard let owner, owner.services === services else { return }
                _ = owner.store.send(.logout)
            } })
        client.selectMac = { id in try await workspace.select(choice: id) }
        client.macStatuses = {
            var statuses = await account.macStatuses()
            let thisMac = await MainActor.run { (workspace.thisMacChoiceID, workspace.thisMac) }
            if let local = thisMac.1 {
                statuses[thisMac.0] = (try? await local.checkSettingsConnection()) != nil ? .route(.thisMac) : .problem(.alondraNotRunning)
            }
            return statuses
        }
        client.forgetMac = { id in try await account.forgetMac(choice: id) }
        if let thisMac = workspace.thisMac {
            client.devices = { try await thisMac.trustedDevices() }
            client.revokeDevice = { id in try await thisMac.revokeDevice(id) }
        }
        client.problem = { services.connectionProblem(for: $0) }
        guard !Task.isCancelled else { return }
        var state = SettingsFeature.State(service: api.base.host() ?? api.base.absoluteString,
            device: api.device, supportsNotifications: false, usesAccount: owner != nil && !services.isDirectOnly)
        state.macs = workspace.macChoices
        state.selectedMacID = workspace.selectedChoiceID
        store = Store(initialState: state) { SettingsFeature() } withDependencies: {
                $0.serviceSettings = client
                $0.cloudPairing = account.additionalMacPairing
                $0.findMac = account.findMacClient
            }
    }
}
