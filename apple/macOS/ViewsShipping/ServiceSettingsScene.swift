import AlondraViewsAppComposition
import AlondraViewsWorkspaceServices
import AlondraCloudPairingFeature
import AlondraSettingsFeature
import ComposableArchitecture
import Observation
import SwiftUI

struct MacServiceSettingsScene: View {
    let services: CloudAppServices
    let owner: CloudPairingOwner?
    @State private var context = MacSettingsContext()
    var body: some View {
        Group {
            if let store = context.store { ServiceSettingsView(store: store) { pairing in
                MacServicePairingScreen(store: pairing).frame(minWidth: 600, minHeight: 600)
            } }
            else { ProgressView("Loading settings…") }
        }.task { await context.connect(services: services, owner: owner) }
    }
}

@MainActor @Observable private final class MacSettingsContext {
    private(set) var store: StoreOf<ServiceSettingsFeature>?
    func connect(services: CloudAppServices, owner: CloudPairingOwner?) async {
        guard store == nil else { return }
        let api = await services.api
        var client = ServiceSettingsClient(observe: { AsyncStream { $0.finish() } },
            save: { _, _ in throw CancellationError() }, requestPermission: {}, openSystemSettings: {},
            checkConnection: { try await services.checkSettingsConnection() },
            logout: { await MainActor.run {
                guard let owner, owner.services === services else { return }
                _ = owner.store.send(.logout)
            } })
        client.selectMac = { id in
            guard let owner else { throw CancellationError() }
            try await owner.selectMac(choice: id, expectedSession: services)
        }
        client.problem = { services.connectionProblem(for: $0) }
        guard !Task.isCancelled else { return }
        var state = ServiceSettingsFeature.State(service: api.base.host() ?? api.base.absoluteString,
            device: api.device, supportsNotifications: false, usesAccount: owner != nil)
        if let owner {
            state.macs = owner.macs.map { ServiceMacChoice(id: $0.id, macID: $0.macID) }
            state.selectedMacID = owner.selectedMacID
        }
        store = Store(initialState: state) { ServiceSettingsFeature() } withDependencies: {
                $0.serviceSettings = client
                if let owner { $0.cloudPairing = owner.additionalMacPairing }
            }
    }
}
