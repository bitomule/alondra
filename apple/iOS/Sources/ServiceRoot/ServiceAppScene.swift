import AlondraAttachmentsFeature
import AlondraDemoWorkspace
import AlondraWorkspaceFeature
import AlondraFleetFeature
import AlondraNewWorkFeature
import AlondraAppComposition
import AlondraCloudPairingFeature
import AlondraPullRequestsFeature
import AlondraQuestionsFeature
import AlondraTasksFeature
import AlondraTriageFeature
import ComposableArchitecture
import Observation
import SwiftUI

struct ServiceAppScene: View {
    let owner: CloudPairingOwner
    let notifications: ServiceNotificationRuntime
    @State private var demo = Store(initialState: DemoModeFeature.State()) { DemoModeFeature() }
    @State private var restoringWorkspace = StoreOf<ServiceRootFeature>(initialState: ServiceRootFeature.State()) { EmptyReducer() }
    var body: some View {
        Group {
        if demo.sessionID != nil {
            DemoWorkspaceScreen(mode: demo) { store in
                ServiceRootView(store: store, accountContent: DemoAccountView())
            }
        } else if let services = owner.services {
            ApprovedServiceScene(services: services, owner: owner, notifications: notifications)
                .id(ObjectIdentifier(services))
        } else if owner.store.showsWorkspace {
            ServiceRootView(store: restoringWorkspace, accountContent: WorkspaceRestorationView(store: owner.store))
                .disabled(true)
                .safeAreaInset(edge: .bottom) { WorkspaceRestorationView(store: owner.store) }
        } else {
            NavigationStack { CloudPairingScreen(store: owner.store) }
                .safeAreaInset(edge: .bottom) {
                    if owner.store.phase == .signedOut { DemoEntryButton(store: demo).padding() }
                }
        }
        }.task { await owner.store.send(.appeared).finish() }
    }
}

/// Dependency assembly belongs to scene composition, outside rendering and reducers.
@MainActor @Observable
private final class ServiceSceneContext {
    private(set) var store: StoreOf<ServiceRootFeature>?

    func consumeNotification(_ notifications: ServiceNotificationRuntime, active: Bool) {
        guard active, let store, let route = notifications.consumeRoute() else { return }
        store.send(.focusInbox(route.itemID))
    }

    func connect(_ services: CloudAppServices) async {
        let triage = await services.triageService()
        let questions = await services.questionWorkService()
        let tasks = await services.taskOperations()
        let schedules = await services.scheduleOperations()
        let prs = await services.pullRequestOperations()
        let nodes = await services.nodeOperations()
        let newWork = await services.newWorkOperations()
        let downloads = await services.attachmentDownloadClient()
        guard !Task.isCancelled else { return }
        store = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() } withDependencies: {
            $0.triageService = triage
            $0.questionWorkService = questions
            $0.taskOperations = tasks
            $0.scheduleOperations = schedules
            $0.pullRequestOperations = prs
            $0.serviceCommandRecovery = ServiceCommandRecoveryClient(observe: { await services.commandResolutions() },
                observeFeedback: { await services.reportFeedbackRecoveryEvents() })
            $0.attachmentDownload = downloads
            $0.attachmentImport = CloudAppServices.attachmentImportClient()
            $0.attachmentUpload = AttachmentUploadClient(uploadWithProgress: { draft, progress in
                try await services.uploadAttachment(draft.data, name: draft.name, transferID: draft.id, progress: progress)
            })
            $0.nodeOperations = nodes
            $0.newWorkOperations = newWork
            $0.serviceTasksClient = ServiceTasksClient(refresh: { await services.sync.refresh() }, observe: { await services.taskSnapshots() })
        }
    }
}

private struct ApprovedServiceScene: View {
    let services: CloudAppServices
    let owner: CloudPairingOwner
    let notifications: ServiceNotificationRuntime
    @Environment(\.scenePhase) private var phase
    // This scene's context is discarded whenever the approved service identity changes.
    @State private var context = ServiceSceneContext()
    var body: some View {
        Group {
            if let store = context.store { ServiceRootView(store: store, accountContent: ServiceSettingsScene(services: services, owner: owner, notifications: notifications)) }
            else { ProgressView("Opening workspace…") }
        }
        .safeAreaInset(edge: .bottom) {
            if let error = notifications.statusError { Text(error).font(.footnote).padding().accessibilityLabel("Notifications: " + error) }
        }
        .task {
            await context.connect(services)
            context.consumeNotification(notifications, active: phase == .active)
        }
        .onChange(of: notifications.push.route) { _, _ in
            context.consumeNotification(notifications, active: phase == .active)
        }
        .onChange(of: phase) { _, phase in
            context.consumeNotification(notifications, active: phase == .active)
        }
    }
}

private struct ServiceRootView<AccountContent: View>: View {
    @Bindable var store: StoreOf<ServiceRootFeature>
    let accountContent: AccountContent
    var body: some View {
        TabView(selection: Binding<ServiceRootFeature.State.Tab>(get: { store.tab }, set: { store.send(.tabSelected($0)) })) {
            Tab("Waiting for You", systemImage: "tray.full", value: ServiceRootFeature.State.Tab.inbox) {
                NavigationStack {
                    ServiceTriageView(store: store.scope(state: \.triage, action: \.triage))
                        .toolbar { ToolbarItem(placement: .topBarTrailing) {
                            Button("Settings", systemImage: "gearshape") { store.send(.settingsPresented(true)) }
                        } }
                }
            }.badge(store.triage.triage.queue.count)
            Tab("Tasks", systemImage: "checklist", value: ServiceRootFeature.State.Tab.tasks) {
                ServiceTasksView(store: store.scope(state: \.tasks, action: \.tasks), focusInbox: { store.send(.focusInbox($0)) }, showSettings: { store.send(.settingsPresented(true)) })
            }
            Tab("Fleet", systemImage: "circle.hexagongrid", value: ServiceRootFeature.State.Tab.fleet) {
                ServiceFleetView(store: store.scope(state: \.fleet, action: \.fleet), focusInbox: { store.send(.focusInbox($0)) })
            }
            Tab("New", systemImage: "plus.bubble", value: ServiceRootFeature.State.Tab.newRequest) {
                NavigationStack { ServiceNewRequestView(store: store.scope(state: \.newRequest, action: \.newRequest)) }
            }
        }
        .tabBarMinimizeBehavior(.onScrollDown)
        .sheet(isPresented: Binding(get: { store.settingsPresented }, set: { store.send(.settingsPresented($0)) })) {
            NavigationStack {
                accountContent
                    .toolbar { ToolbarItem(placement: .cancellationAction) {
                        Button("Close", systemImage: "xmark") { store.send(.settingsPresented(false)) }
                    } }
            }.presentationDetents([.medium, .large])
        }
        .task { await store.send(.appeared).finish() }
        .onDisappear { store.send(.disappeared) }
        .sheet(item: $store.scope(state: \.pullRequest, action: \.pullRequest)) { child in
            NavigationStack { ServicePullRequestView(store: child) }
        }
    }
}
