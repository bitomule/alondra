import AlondraAttachmentsFeature
import AlondraCloudPairingFeature
import AlondraConnectionFeature
import AlondraHomeFeature
import AlondraNetworkReachability
import AlondraShared
import AlondraViewsAppComposition
import AlondraViewsDependency
import AlondraViewsDemoWorkspace
import AlondraViewsWorkspaceFeature
import AlondraViewsWorkspaceServices
import ComposableArchitecture
import Observation
import SwiftUI

struct ServiceAppScene: View {
    let owner: CloudPairingOwner
    let notifications: ServiceNotificationRuntime
    @State private var demoSession: UUID?

    var body: some View {
        Group {
        if let demoSession {
            ViewsDemoScene(reset: { self.demoSession = UUID() }, exit: { self.demoSession = nil })
                .id(demoSession)
        } else {
        CloudWorkspaceGate(store: owner.store, hasServices: owner.services != nil) {
            if let services = owner.services {
                ApprovedServiceScene(services: services, owner: owner, notifications: notifications)
                    .id(ObjectIdentifier(services))
            }
        } restoring: {
            RestoreConnectionView(store: owner.store, networkChanges: { NetworkReachability.shared.changes() })
                .background(DawnBackground())
        } onboarding: {
            NavigationStack { CloudPairingScreen(store: owner.store) }
                .safeAreaInset(edge: .bottom) {
                    if owner.store.phase == .signedOut {
                        Button("Explore the demo") { demoSession = UUID() }
                            .accessibilityIdentifier("demo.enter")
                            .padding()
                    }
                }
        }
        }
        }
        .task { await owner.store.send(.appeared).finish() }
    }
}

private struct ViewsDemoScene: View {
    let reset: () -> Void
    let exit: () -> Void
    @State private var store: StoreOf<ViewsServiceRootFeature>

    init(reset: @escaping () -> Void, exit: @escaping () -> Void) {
        self.reset = reset
        self.exit = exit
        _store = State(initialValue: Store(initialState: ViewsServiceRootFeature.State()) {
            ViewsServiceRootFeature()
        } withDependencies: {
            $0.viewsClient = ViewsDemoWorkspace.client()
        })
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Label("Demo · Sample data", systemImage: "sparkles")
                Spacer()
                Button("Reset demo", action: reset).accessibilityIdentifier("demo.reset")
                Button("Exit demo", action: exit).accessibilityIdentifier("demo.exit")
            }.font(.callout).padding().background(.thinMaterial)
            ServiceRootView(store: store, accountContent: ViewsDemoAccountView())
        }
        .environment(\.openURL, OpenURLAction { _ in .discarded })
    }
}

private struct ViewsDemoAccountView: View {
    var body: some View {
        ContentUnavailableView("Demo workspace", systemImage: "sparkles",
            description: Text("Everything here is sample data. Actions stay in memory and never reach a real account. Use Reset demo to start over or Exit demo to connect your workspace."))
    }
}

@MainActor @Observable private final class ServiceSceneContext {
    let settings = ServiceSettingsContext()
    private(set) var store: StoreOf<ViewsServiceRootFeature>?

    func consumeNotification(_ notifications: ServiceNotificationRuntime, services: CloudAppServices, active: Bool) {
        guard active, let store, let route = notifications.consumeRoute(for: services) else { return }
        store.send(.focusInbox(route.itemID))
    }

    func connect(_ services: CloudAppServices) async {
        let viewsFeed = await services.viewsFeed
        let sender = await services.viewActionSender
        guard !Task.isCancelled else { return }
        store = Store(initialState: ViewsServiceRootFeature.State()) { ViewsServiceRootFeature() } withDependencies: {
            $0.viewsClient = ViewsClient.live(session: viewsFeed, sender: sender)
            $0.attachmentImport = CloudAppServices.attachmentImportClient()
            $0.attachmentUpload = AttachmentUploadClient(uploadWithProgress: { draft, progress in
                try await services.uploadAttachment(draft.data, name: draft.name, transferID: draft.id, progress: progress)
            })
        }
    }
}

private struct ApprovedServiceScene: View {
    let services: CloudAppServices
    let owner: CloudPairingOwner
    let notifications: ServiceNotificationRuntime
    @Environment(\.scenePhase) private var phase
    @State private var context = ServiceSceneContext()

    var body: some View {
        Group {
            if let store = context.store {
                ServiceRootView(store: store,
                    accountContent: ServiceSettingsScene(services: services, owner: owner,
                                                        notifications: notifications, context: context.settings))
                    .connectionLostOverlay(store: store, retry: retry,
                                           signIn: { owner.store.send(.reauthenticate) })
            } else { ProgressView("Opening workspace…") }
        }
        .retriesConnection(networkChanges: { NetworkReachability.shared.changes() }) { retry() }
        .onChange(of: owner.store.restorationInFlight) { wasSigningIn, signingIn in
            if wasSigningIn && !signingIn { retry() }
        }
        .safeAreaInset(edge: .bottom) {
            if let error = notifications.statusError {
                Text(error).font(.footnote).padding().accessibilityLabel("Notifications: " + error)
            }
        }
        .task {
            await context.connect(services)
            context.consumeNotification(notifications, services: services, active: phase == .active)
        }
        .onChange(of: notifications.push.route) { _, _ in
            context.consumeNotification(notifications, services: services, active: phase == .active)
        }
        .onChange(of: phase) { _, phase in
            context.consumeNotification(notifications, services: services, active: phase == .active)
        }
    }

    private func retry() { Task { await services.retryViewsConnection() } }
}

struct ServiceRootView<AccountContent: View>: View {
    @Bindable var store: StoreOf<ViewsServiceRootFeature>
    let accountContent: AccountContent
    @Namespace private var triageZoom

    var body: some View {
        TabView(selection: Binding<ViewsServiceRootFeature.State.Tab>(get: { store.tab }, set: { store.send(.tabSelected($0)) })) {
            Tab("Waiting for You", systemImage: "tray.full", value: .inbox) {
                NavigationStack(path: $store.scope(state: \.serverReviewPath, action: \.serverReviewPath)) {
                    ServiceHomeView(store: store.scope(state: \.home, action: \.home), zoom: triageZoom,
                        openReview: { store.send(.openServerView($0)) })
                        .toolbar { ToolbarItem(placement: .topBarTrailing) {
                            Button("Settings", systemImage: "gearshape") { store.send(.settingsPresented(true)) }
                        } }
                } destination: { child in
                    ViewsScreenView(store: child, openView: { store.send(.openReviewView($0)) })
                }
            }.badge(store.home.badge)
            Tab("Tasks", systemImage: "checklist", value: .tasks) {
                NavigationStack(path: $store.scope(state: \.serverTasksPath, action: \.serverTasksPath)) {
                    ViewsScreenView(store: store.scope(state: \.serverTasks, action: \.serverTasks),
                                    openView: { store.send(.openTasksView($0)) })
                        .toolbar { ToolbarItem(placement: .topBarTrailing) {
                            Button("Settings", systemImage: "gearshape") { store.send(.settingsPresented(true)) }
                        } }
                } destination: { child in
                    ViewsScreenView(store: child, openView: { store.send(.openTasksView($0)) })
                }
            }
            Tab("Fleet", systemImage: "circle.hexagongrid", value: .fleet) {
                NavigationStack(path: $store.scope(state: \.serverFleetPath, action: \.serverFleetPath)) {
                    ViewsScreenView(store: store.scope(state: \.serverFleet, action: \.serverFleet),
                                    openView: { store.send(.openFleetView($0)) })
                } destination: { child in
                    ViewsScreenView(store: child, openView: { store.send(.openFleetView($0)) })
                }
            }
            Tab("New", systemImage: "plus.bubble", value: .newRequest) {
                NavigationStack(path: $store.scope(state: \.serverNewRequestPath, action: \.serverNewRequestPath)) {
                    ViewsScreenView(store: store.scope(state: \.serverNewRequest, action: \.serverNewRequest),
                                    openView: { store.send(.openNewRequestView($0)) })
                } destination: { child in
                    ViewsScreenView(store: child, openView: { store.send(.openNewRequestView($0)) })
                }
            }
        }
        .overlay(alignment: .bottom) { ServiceNoticeToast(store: store) }
        .closeRefusalAlert(Binding(get: { store.closeRefusal }, set: {
            if $0 == nil { store.send(.dismissCloseRefusal) }
        }))
        .tabBarMinimizeBehavior(.onScrollDown)
        .sheet(isPresented: Binding(get: { store.settingsPresented }, set: { store.send(.settingsPresented($0)) })) {
            NavigationStack {
                accountContent.toolbar { ToolbarItem(placement: .cancellationAction) {
                    Button("Close", systemImage: "xmark") { store.send(.settingsPresented(false)) }
                } }
            }.presentationDetents([.medium, .large])
        }
        .task { await store.send(.appeared).finish() }
        .onDisappear { store.send(.disappeared) }
    }
}
