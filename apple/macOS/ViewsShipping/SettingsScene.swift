import AlondraAccount
import AlondraFeatures
import ComposableArchitecture
import Observation
import SwiftUI

struct MacServiceSettingsScene: View {
    let services: Connection
    let owner: AccountSession?
    let workspace: MacWorkspaceOwner
    let context: MacSettingsContext
    var body: some View {
        Group {
            if let store = context.store { MacSettingsView(store: store) { pairing in
                MacServicePairingScreen(store: pairing).frame(minWidth: 600, minHeight: 600)
            } }
            else { ProgressView("Loading settings…") }
        }.task { await context.connect(services: services, owner: owner, workspace: workspace) }
    }
}

@MainActor @Observable final class MacSettingsContext {
    private(set) var store: StoreOf<SettingsFeature>?
    private var pairingRequested: (() -> Void)?

    /// Opens the pairing sheet as soon as the settings exist, for a Mac the person pairs again.
    /// `unopened` runs when the sheet did not open, so no pairing-again request is left behind.
    func requestLinkMac(unopened: @escaping () -> Void) {
        if store != nil { openPairing(unopened) } else { pairingRequested = unopened }
    }

    private func openPairing(_ unopened: () -> Void) {
        guard let store else { return unopened() }
        store.send(.linkMac)
        if store.pairing == nil { unopened() }
    }

    func connect(services: Connection, owner: AccountSession?, workspace: MacWorkspaceOwner) async {
        guard store == nil else { return }
        let api = await services.api
        let account = workspace.account
        var client = SettingsClient(observe: { AsyncStream { $0.finish() } },
            save: { _, _ in throw CancellationError() }, requestPermission: {}, openSystemSettings: {},
            checkConnection: { try await services.checkSettingsConnection() },
            logout: { await MainActor.run {
                if let owner { guard owner.services === services else { return } }
                _ = account.store.send(.logout)
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
        state.showsSignOutWithMacs = owner == nil && account.macs.contains { !$0.local }
        store = Store(initialState: state) { SettingsFeature() } withDependencies: {
                $0.serviceSettings = client
                $0.cloudPairing = account.additionalMacPairing
                $0.findMac = account.findMacClient
            }
        if let unopened = pairingRequested {
            pairingRequested = nil
            openPairing(unopened)
        }
    }
}
