import AlondraContract
import AlondraAttachments
import AlondraFeatures
import AlondraNetworkReachability
import AlondraDesign
import AlondraAccount
import AlondraFeed
import AlondraDemo
import ComposableArchitecture
import Observation
import SwiftUI

struct AppScene: View {
    let owner: AccountSession
    let notifications: NotificationRuntime
    @State private var demoSession: UUID?

    var body: some View {
        Group {
        if let demoSession {
            DemoScene(reset: { self.demoSession = UUID() }, exit: { self.demoSession = nil })
                .id(demoSession)
        } else {
        LaunchGate(store: owner.store, hasServices: owner.services != nil) {
            if let services = owner.services {
                ApprovedServiceScene(services: services, owner: owner, notifications: notifications)
                    .id(ObjectIdentifier(services))
            }
        } restoring: {
            RestoreConnectionView(store: owner.store, networkChanges: { NetworkReachability.shared.changes() })
                .background(DawnBackground())
        } onboarding: {
            NavigationStack { SignInView(store: owner.store) }
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

private struct DemoScene: View {
    let reset: () -> Void
    let exit: () -> Void
    @State private var store: StoreOf<AppFeature>

    init(reset: @escaping () -> Void, exit: @escaping () -> Void) {
        self.reset = reset
        self.exit = exit
        _store = State(initialValue: Store(initialState: AppFeature.State()) {
            AppFeature()
        } withDependencies: {
            $0.viewsClient = ViewsClient.demo()
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
            RootView(store: store, accountContent: DemoAccountView())
        }
        .environment(\.openURL, OpenURLAction { _ in .discarded })
    }
}

private struct DemoAccountView: View {
    var body: some View {
        ContentUnavailableView("Demo workspace", systemImage: "sparkles",
            description: Text("Everything here is sample data. Actions stay in memory and never reach a real account. Use Reset demo to start over or Exit demo to connect your workspace."))
    }
}

@MainActor @Observable private final class SceneContext {
    let settings = SettingsContext()
    private(set) var store: StoreOf<AppFeature>?

    func consumeNotification(_ notifications: NotificationRuntime, services: Connection, active: Bool) {
        guard active, let store, let route = notifications.consumeRoute(for: services) else { return }
        store.send(.notificationOpened(cardID: route.itemID))
    }

    func connect(_ services: Connection) async {
        let viewsFeed = await services.viewsFeed
        let sender = await services.viewActionSender
        guard !Task.isCancelled else { return }
        store = Store(initialState: AppFeature.State()) { AppFeature() } withDependencies: {
            $0.viewsClient = ViewsClient.live(session: viewsFeed, sender: sender)
            $0.attachmentImport = Connection.attachmentImportClient()
            $0.attachmentUpload = AttachmentUploadClient(uploadWithProgress: { draft, progress in
                try await services.uploadAttachment(draft.data, name: draft.name, transferID: draft.id, progress: progress)
            })
        }
    }
}

private struct ApprovedServiceScene: View {
    let services: Connection
    let owner: AccountSession
    let notifications: NotificationRuntime
    @Environment(\.scenePhase) private var phase
    @State private var context = SceneContext()

    var body: some View {
        Group {
            if let store = context.store {
                RootView(store: store,
                    accountContent: SettingsScene(services: services, owner: owner,
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

struct RootView<AccountContent: View>: View {
    @Bindable var store: StoreOf<AppFeature>
    let accountContent: AccountContent
    @Namespace private var triageZoom

    var body: some View {
        TabView(selection: Binding<AppFeature.State.Tab>(get: { store.tab }, set: { store.send(.tabSelected($0)) })) {
            Tab("Waiting for You", systemImage: "tray.full", value: .inbox) {
                NavigationStack(path: $store.scope(state: \.inboxPath, action: \.inboxPath)) {
                    TriageView(store: store.scope(state: \.home, action: \.home), zoom: triageZoom,
                        openReview: { store.send(.openFromTriage(ViewKey(rawValue: $0))) })
                        .toolbar { ToolbarItem(placement: .topBarTrailing) {
                            Button("Settings", systemImage: "gearshape") { store.send(.settingsPresented(true)) }
                        } }
                } destination: { route in
                    detail(route, tab: .inbox)
                }
            }.badge(store.home.badge)
            Tab("Tasks", systemImage: "checklist", value: .tasks) {
                NavigationStack(path: $store.scope(state: \.tasksPath, action: \.tasksPath)) {
                    ViewsScreenView(store: store.scope(state: \.tasks, action: \.tasks),
                                    openView: { store.send(.open(.tasks, $0)) })
                        .toolbar { ToolbarItem(placement: .topBarTrailing) {
                            Button("Settings", systemImage: "gearshape") { store.send(.settingsPresented(true)) }
                        } }
                } destination: { route in
                    detail(route, tab: .tasks)
                }
            }
            Tab("Fleet", systemImage: "circle.hexagongrid", value: .fleet) {
                NavigationStack(path: $store.scope(state: \.fleetPath, action: \.fleetPath)) {
                    ViewsScreenView(store: store.scope(state: \.fleet, action: \.fleet),
                                    openView: { store.send(.open(.fleet, $0)) })
                } destination: { route in
                    detail(route, tab: .fleet)
                }
            }
            Tab("New", systemImage: "plus.bubble", value: .newRequest) {
                NavigationStack(path: $store.scope(state: \.newRequestPath, action: \.newRequestPath)) {
                    ViewsScreenView(store: store.scope(state: \.newRequest, action: \.newRequest),
                                    openView: { store.send(.open(.newRequest, $0)) })
                } destination: { route in
                    detail(route, tab: .newRequest)
                }
            }
        }
        .overlay(alignment: .bottom) { NoticeToast(store: store) }
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

    @ViewBuilder private func detail(_ route: Store<DetailRoute.State, DetailRoute.Action>, tab: AppFeature.State.Tab) -> some View {
        switch route.case {
        case .legacy(let screen):
            ViewsScreenView(store: screen, openView: { store.send(.open(tab, $0)) })
        }
    }
}
