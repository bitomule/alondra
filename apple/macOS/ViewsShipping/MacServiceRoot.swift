import AlondraShared
import AlondraAttachmentsFeature
import AlondraViewsWorkspaceFeature
import AlondraViewsAppComposition
import AlondraViewsWorkspaceServices
import AlondraViewsDemoWorkspace
import AlondraCloudPairingFeature
import AlondraConnectionFeature
import AlondraNetworkReachability
import AlondraViewsDependency
import ComposableArchitecture
import Observation
import SwiftUI

struct MacServiceRoot: View {
    let owner: MacWorkspaceOwner
    @State private var connectingRemote = false
    @State private var demoSession: UUID?
    private var account: CloudPairingOwner { owner.account }
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
                    MacServiceWindow(services: services, account: nil)
                        .id(ObjectIdentifier(services))
                }
            case .remote:
                CloudWorkspaceGate(store: account.store, hasServices: account.services != nil) {
                    if let services = account.services {
                        MacServiceWindow(services: services, account: account)
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
        }.task { await owner.run() }
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
    @State private var store = Store(initialState: ViewsServiceRootFeature.State()) {
        ViewsServiceRootFeature()
    } withDependencies: {
        $0.viewsClient = ViewsDemoWorkspace.client()
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
    private(set) var store: StoreOf<ViewsServiceRootFeature>?
    func connect(_ services: CloudAppServices) async {
        guard store == nil else { return }
        let viewsFeed = await services.viewsFeed
        let viewActionSender = await services.viewActionSender
        guard !Task.isCancelled else { return }
        store = Store(initialState: ViewsServiceRootFeature.State()) { ViewsServiceRootFeature() } withDependencies: {
            $0.viewsClient = ViewsClient.live(session: viewsFeed, sender: viewActionSender)
            $0.attachmentImport = CloudAppServices.attachmentImportClient()
            $0.attachmentUpload = AttachmentUploadClient(uploadWithProgress: { draft, progress in
                try await services.uploadAttachment(draft.data, name: draft.name, transferID: draft.id, progress: progress)
            })
        }
    }
}

private struct MacServiceWindow: View {
    let services: CloudAppServices
    let account: CloudPairingOwner?
    @State private var context = MacServiceWindowContext()
    var body: some View {
        Group {
            if let store = context.store {
                MacServiceWorkspace(store: store, accountContent: MacServiceSettingsScene(services: services, owner: account))
                    .connectionLostOverlay(store: store, retry: retry,
                                           signIn: account.map { account in { account.store.send(.reauthenticate) } })
            } else {
                ProgressView("Opening workspace…")
            }
        }
        .task { await context.connect(services) }
        .retriesConnection(networkChanges: { NetworkReachability.shared.changes() }) { retry() }
        .onChange(of: account?.store.restorationInFlight) { wasSigningIn, signingIn in
            if wasSigningIn == true && signingIn == false { retry() }
        }
    }

    private func retry() {
        Task { await services.retryViewsConnection() }
    }
}
