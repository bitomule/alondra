import AlondraDesign
import AlondraAttachments
import AlondraFeatures
import AlondraMacWorkspace
import AlondraAccount
import AlondraDemo
import AlondraNetworkReachability
import AlondraFeed
import ComposableArchitecture
import Observation
import SwiftUI

struct MacServiceRoot: View {
    let owner: MacWorkspaceOwner
    let notices: MacLocalNotices
    @Environment(\.openWindow) private var openWindow
    @State private var connectingRemote = false
    @State private var demoSession: UUID?
    private var account: AccountSession { owner.account }
    var body: some View {
        Group {
            if let demoSession {
                MacViewsDemoScene(reset: { self.demoSession = UUID() }, exit: { self.demoSession = nil })
                    .id(demoSession)
            } else {
            switch owner.route {
            case .searching:
                ProgressView("Looking for Alondra on this Mac…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            case .local:
                if let services = owner.services {
                    MacServiceWindow(services: services, account: owner.showsThisMac ? nil : account,
                                     workspace: owner, notices: notices)
                        .id(ObjectIdentifier(services))
                }
            case .remote:
                LaunchGate(store: account.store, hasServices: account.services != nil) {
                    if let services = account.services {
                        MacServiceWindow(services: services, account: account, workspace: owner, notices: notices)
                            .id(ObjectIdentifier(services))
                    }
                } restoring: {
                    RestoreConnectionView(store: account.store, networkChanges: { NetworkReachability.shared.changes() })
                } onboarding: {
                    if connectingRemote || account.store.phase != .signedOut {
                        MacServicePairingScreen(store: account.store)
                            .safeAreaInset(edge: .bottom) {
                                if account.store.phase == .signedOut { demoButton }
                            }
                    } else {
                        MacLocalDaemonMissingView(connectToAnotherMac: { connectingRemote = true },
                                                  exploreDemo: { demoSession = UUID() })
                    }
                }
            }
            }
        }
        .task { await owner.run() }
        .onAppear { notices.openWindow = openWindow }
        .sheet(item: Binding(get: { notices.shownPairing }, set: { shown in
            if shown == nil, let id = notices.shownPairing?.id { notices.dismissedPairings.insert(id) }
        })) { request in
            MacPairingRequestSheet(request: request, error: notices.pairingError) { allow in
                Task { await notices.decide(request.id, allow: allow) }
            }
        }
    }

    private var demoButton: some View {
        Button("Explore the demo") { demoSession = UUID() }
            .accessibilityIdentifier("demo.enter")
            .padding()
    }
}

private struct MacViewsDemoScene: View {
    let reset: () -> Void
    let exit: () -> Void
    @State private var store = Store(initialState: MacAppFeature.State()) {
        MacAppFeature()
    } withDependencies: {
        $0.viewsClient = ViewsClient.demo()
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Label("Demo · Sample data", systemImage: "sparkles")
                Spacer()
                Button("Reset demo", action: reset).accessibilityIdentifier("demo.reset")
                Button("Exit demo", action: exit).accessibilityIdentifier("demo.exit")
            }
            .font(.callout).padding().background(.thinMaterial)
            MacServiceWorkspace(store: store, accountContent: DemoAccountView())
        }
        .environment(\.openURL, OpenURLAction { _ in .discarded })
    }
}

private struct DemoAccountView: View {
    var body: some View {
        ContentUnavailableView("Demo workspace", systemImage: "sparkles",
            description: Text("Everything here is sample data. Actions stay in memory and never reach a real account."))
    }
}

/// Scene dependency assembly; authentication and I/O are process-owned by the account.
@MainActor @Observable
private final class MacServiceWindowContext {
    let settings = MacSettingsContext()
    private(set) var store: StoreOf<MacAppFeature>?
    func connect(_ services: Connection) async {
        guard store == nil else { return }
        let viewsFeed = await services.viewsFeed
        let viewActionSender = await services.viewActionSender
        guard !Task.isCancelled else { return }
        store = Store(initialState: MacAppFeature.State()) { MacAppFeature() } withDependencies: {
            $0.viewsClient = ViewsClient.live(session: viewsFeed, sender: viewActionSender)
            $0.attachmentImport = Connection.attachmentImportClient()
            $0.attachmentUpload = AttachmentUploadClient(uploadWithProgress: { draft, progress in
                try await services.uploadAttachment(draft.data, name: draft.name, transferID: draft.id, progress: progress)
            })
        }
    }
}

private struct MacServiceWindow: View {
    let services: Connection
    let account: AccountSession?
    let workspace: MacWorkspaceOwner
    let notices: MacLocalNotices
    @State private var context = MacServiceWindowContext()
    var body: some View {
        Group {
            if let store = context.store {
                MacServiceWorkspace(store: store, accountContent: MacServiceSettingsScene(services: services, owner: account,
                                                                                         workspace: workspace, context: context.settings))
                    .connectionLostOverlay(store: store, diagnose: { await services.diagnose() }, retry: retry,
                                           signIn: account.map { account in { account.store.send(.reauthenticate) } },
                                           pairAgain: account.flatMap { account in
                                               account.pairAgainAction(for: services) {
                                                   context.settings.requestLinkMac(unopened: { account.pairAgain(macID: nil) })
                                                   store.send(.settingsPresented(true))
                                               }
                                           },
                                           localNetworkSettings: account.map { account in { account.openLocalNetworkSettings() } })
                    .onChange(of: notices.openedCard, initial: true) { _, card in
                        guard let card else { return }
                        store.send(.macPresentation(.select(.viewKey("card:\(card)"))))
                        notices.openedCard = nil
                    }
            } else {
                ProgressView("Opening workspace…")
            }
        }
        .task { await context.connect(services) }
        .retriesConnection(networkChanges: { NetworkReachability.shared.changes() },
                           hints: { NetworkReachability.shared.retryHints() }) { retry() }
        .onChange(of: account?.store.restorationInFlight) { wasSigningIn, signingIn in
            if wasSigningIn == true && signingIn == false { retry() }
        }
    }

    private func retry() {
        Task { await services.retryViewsConnection() }
    }
}
