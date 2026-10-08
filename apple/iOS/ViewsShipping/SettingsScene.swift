import AlondraiOSUI
import AlondraAccount
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Observation
import SwiftUI

struct SettingsScene: View {
    let services: Connection
    let owner: AccountSession
    let notifications: NotificationRuntime
    let context: SettingsContext
    var body: some View {
        Group {
            if let store = context.store { SettingsView(store: store) }
            else { ProgressView("Loading settings…") }
        }.task { await context.connect(services: services, owner: owner, notifications: notifications) }
    }
}

@MainActor @Observable final class SettingsContext {
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

    func connect(services: Connection, owner: AccountSession, notifications: NotificationRuntime) async {
        guard store == nil else { return }
        let api = await services.api
        let viewsFeed = await services.viewsFeed
        let sender = await services.viewActionSender
        let client = notifications.settingsClient(services: services, owner: owner)
        guard !Task.isCancelled else { return }
        // Without the relay there is no push, so there is nothing to turn on here.
        let usesAccount = owner.macs.contains { !$0.local }
        var state = SettingsFeature.State(service: api.base.host() ?? api.base.absoluteString,
            device: api.device, supportsNotifications: usesAccount, automaticallySavesQuietHours: true,
            usesAccount: usesAccount)
        state.macs = owner.macs
        state.selectedMacID = owner.selectedMacID
        store = Store(initialState: state) { SettingsFeature() } withDependencies: {
                $0.serviceSettings = client
                $0.viewsClient = ViewsClient.live(session: viewsFeed, sender: sender)
                $0.cloudPairing = owner.additionalMacPairing
                $0.findMac = owner.findMacClient
            }
        if let unopened = pairingRequested {
            pairingRequested = nil
            openPairing(unopened)
        }
    }
}
