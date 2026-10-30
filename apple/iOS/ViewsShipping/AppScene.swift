import AlondraContract
import AlondraAttachments
import AlondraFeatures
import AlondraNetworkReachability
import AlondraDesign
import AlondraAccount
import AlondraFeed
import AlondraDemo
import AlondraLiveActivity
import AlondraiOSUI
import ComposableArchitecture
import Observation
import SwiftUI
import UserNotifications

struct AppScene: View {
    let owner: AccountSession
    let notifications: NotificationRuntime
    let liveActivity: LiveActivityRuntime
    @Environment(\.scenePhase) private var phase
    @State private var demoSession: UUID?

    var body: some View {
        Group {
        if let demoSession {
            DemoScene(reset: { self.demoSession = UUID() }, exit: { self.demoSession = nil })
                .id(demoSession)
        } else {
        LaunchGate(store: owner.store, hasServices: owner.services != nil) {
            if let services = owner.services {
                ApprovedServiceScene(services: services, owner: owner, notifications: notifications, liveActivity: liveActivity)
                    .id(ObjectIdentifier(services))
            }
        } restoring: {
            RestoreConnectionView(store: owner.store, networkChanges: { NetworkReachability.shared.changes() })
                .background(DawnBackground())
        } onboarding: {
            NavigationStack { SignInView(store: owner.store, demo: { demoSession = UUID() }) }
                .onAppear { liveActivity.controller.keep(macIDs: []) }
        }
        }
        }
        .task { await owner.store.send(.appeared).finish() }
        .onChange(of: phase, initial: true) { _, phase in
            switch phase {
            case .active:
                liveActivity.controller.enteredForeground()
                Task { await owner.goLive() }
            case .background:
                liveActivity.controller.enteredBackground()
                Task { await owner.goIdle() }
            default: break
            }
        }
        .onChange(of: owner.macs.map(\.macID)) { _, macIDs in
            if !macIDs.isEmpty { liveActivity.controller.keep(macIDs: Set(macIDs)) }
        }
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
            $0.viewsClient = ViewsClient.demoInDeviceLanguage()
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
    private(set) var storedFiles: StoredFiles?
    private(set) var store: StoreOf<AppFeature>?

    private var pendingLink: AlondraLink?

    func open(_ url: URL) {
        guard let link = AlondraLink(url: url) else { return }
        pendingLink = link
        consumeLink()
    }

    func consumeLink() {
        guard let store, let link = pendingLink else { return }
        pendingLink = nil
        store.send(.linkOpened(link))
    }

    func consumeNotification(_ notifications: NotificationRuntime, services: Connection, active: Bool) {
        guard active, let store, let route = notifications.consumeRoute(for: services) else { return }
        store.send(.notificationOpened(cardID: route.itemID))
    }

    func connect(_ services: Connection) async {
        let viewsFeed = await services.viewsFeed
        let sender = await services.viewActionSender
        guard !Task.isCancelled else { return }
        storedFiles = StoredFiles { try await viewsFeed.fetchAttachment($0) }
        store = Store(initialState: AppFeature.State()) { AppFeature() } withDependencies: {
            $0.viewsClient = ViewsClient.live(session: viewsFeed, sender: sender, monitor: services.monitor)
            $0.mcpSignIn = services.mcpSignInClient(browser: McpSignInBrowser())
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
    let liveActivity: LiveActivityRuntime
    @Environment(\.scenePhase) private var phase
    @State private var context = SceneContext()
    @State private var returningFromBackground = false

    var body: some View {
        Group {
            if let store = context.store {
                RootView(store: store,
                    accountContent: SettingsScene(services: services, owner: owner,
                                                        notifications: notifications, context: context.settings))
                    .connectionLostOverlay(store.connection, hasSavedData: store.triage.screen.document != nil, retry: retry,
                                           signIn: { owner.store.send(.reauthenticate) },
                                           settings: { store.send(.settingsPresented(true)) },
                                           pairAgain: owner.pairAgainAction(for: services) {
                                               context.settings.requestLinkMac(unopened: { owner.pairAgain(macID: nil) })
                                               store.send(.settingsPresented(true))
                                           },
                                           localNetworkSettings: { owner.openLocalNetworkSettings() })
                    .environment(\.connectionRecovery, ConnectionRecovery(retry: retry,
                                                                          localNetworkSettings: { owner.openLocalNetworkSettings() }))
                    .environment(\.storedFiles, context.storedFiles)
                    .environment(\.connectionState, store.connection)
            } else { ProgressView("Opening workspace…") }
        }
        .retriesConnection(networkChanges: { NetworkReachability.shared.changes() },
                           hints: { NetworkReachability.shared.retryHints() }) { retry() }
        .onChange(of: owner.store.restorationInFlight) { wasSigningIn, signingIn in
            if wasSigningIn && !signingIn { retry() }
        }
        .task {
            notifications.macReached = { await services.macReached() }
            await context.connect(services)
            context.consumeLink()
            context.consumeNotification(notifications, services: services, active: phase == .active)
        }
        .onOpenURL { context.open($0) }
        .onChange(of: activityCounts, initial: true) { _, reported in
            guard let reported, let macID = services.macID else { return }
            liveActivity.controller.report(macID: macID, name: owner.macs.first { $0.macID == macID }?.name,
                                           counts: reported.counts, live: reported.live)
        }
        .onChange(of: notifications.push.route) { _, _ in
            context.consumeNotification(notifications, services: services, active: phase == .active)
        }
        .onChange(of: phase) { _, phase in
            if phase == .background { returningFromBackground = true }
            if phase == .active && returningFromBackground {
                returningFromBackground = false
                notifications.push.reregisterOnReturn()
            }
            if phase == .active {
                notifications.drainUnregistrations()
                showIconBadge(context.store?.triage.iconBadge)
                reconcileNotifications()
            }
            context.consumeNotification(notifications, services: services, active: phase == .active)
        }
        .onChange(of: context.store?.triage.iconBadge, initial: true) { _, badge in showIconBadge(badge) }
        .onChange(of: context.store?.triage.orderedIDs) { _, _ in reconcileNotifications() }
    }

    /// What this Mac adds to the Live Activity, once its queue has loaded.
    private var activityCounts: ReportedCounts? {
        guard let triage = context.store?.triage, let inbox = triage.iconBadge else { return nil }
        let count = { (id: String) in Int(triage.status?.counts.first { $0.id == id }?.value ?? 0) }
        return ReportedCounts(counts: ActivityCounts(inbox: inbox, waiting: count("waiting"), working: count("working")),
                              live: context.store?.connection.isConnected == true)
    }

    /// Only a loaded queue is the truth: before it loads it is empty and would clear everything.
    private func reconcileNotifications() {
        guard phase == .active, let triage = context.store?.triage, triage.iconBadge != nil else { return }
        let waiting = triage.orderedIDs
        Task { await notifications.reconcile(waiting: waiting, for: services) }
    }

    private func retry() { Task { await services.retryViewsConnection() } }

    private func showIconBadge(_ badge: Int?) {
        guard let badge else { return }
        Task { try? await UNUserNotificationCenter.current().setBadgeCount(badge) }
    }
}

private struct ReportedCounts: Equatable {
    let counts: ActivityCounts
    let live: Bool
}

struct RootView<AccountContent: View>: View {
    @Bindable var store: StoreOf<AppFeature>
    let accountContent: AccountContent
    @Namespace private var triageZoom

    var body: some View {
        TabView(selection: Binding<AppFeature.State.Tab>(get: { store.tab }, set: { store.send(.tabSelected($0)) })) {
            Tab("Waiting for You", systemImage: "tray.full", value: .inbox) {
                NavigationStack(path: $store.scope(state: \.inboxPath, action: \.inboxPath)) {
                    TriageView(store: store.scope(state: \.triage, action: \.triage), zoom: triageZoom)
                        .toolbar { ToolbarItem(placement: .topBarTrailing) {
                            Button("Settings", systemImage: "gearshape") { store.send(.settingsPresented(true)) }
                        } }
                } destination: { route in
                    detail(route, tab: .inbox)
                }
            }.badge(store.triage.badge)
            Tab("Tasks", systemImage: "checklist", value: .tasks) {
                NavigationStack(path: $store.scope(state: \.tasksPath, action: \.tasksPath)) {
                    TasksView(store: store.scope(state: \.tasks, action: \.tasks))
                        .toolbar { ToolbarItem(placement: .topBarTrailing) {
                            Button("Settings", systemImage: "gearshape") { store.send(.settingsPresented(true)) }
                        } }
                } destination: { route in
                    detail(route, tab: .tasks)
                }
            }
            Tab("New", systemImage: "plus.bubble", value: .newRequest) {
                NavigationStack(path: $store.scope(state: \.newRequestPath, action: \.newRequestPath)) {
                    NewRequestView(store: store.scope(state: \.newRequest, action: \.newRequest))
                } destination: { route in
                    detail(route, tab: .newRequest)
                }
            }
        }
        .overlay(alignment: .bottom) { NoticeToast(store: store) }
        .tabBarMinimizeBehavior(.onScrollDown)
        .environment(\.mcpLinks, McpLinks(signIn: { store.send(.mcpSignInRequested(card: $0, server: $1)) },
                                          reconnect: { store.send(.mcpReconnectRequested(card: $0)) }))
        .sheet(item: $store.scope(state: \.mcpSignIn, action: \.mcpSignIn)) { signIn in
            McpSignInSheet(store: signIn)
        }
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
        case .taskDetail(let screen):
            TaskDetailView(store: screen)
        case .schedules(let screen):
            SchedulesView(store: screen)
        case .scheduleEditor(let screen):
            ScheduleEditorView(store: screen)
        case .prReview(let screen):
            PRReviewView(store: screen)
        case .cardDetail(let review):
            if tab == .inbox {
                CardDetailView(store: review).navigationTransition(.zoom(sourceID: String(review.screen.key.rawValue.dropFirst("card:".count)), in: triageZoom))
            } else {
                CardDetailView(store: review)
            }
        }
    }
}
