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
    var body: some View {
        if let services = owner.services {
            ApprovedServiceScene(services: services, pairing: owner.store, notifications: notifications)
                .id(ObjectIdentifier(services))
        } else {
            NavigationStack { CloudPairingScreen(store: owner.store) }
        }
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
        guard !Task.isCancelled else { return }
        store = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() } withDependencies: {
            $0.triageService = triage
            $0.questionWorkService = questions
            $0.taskOperations = tasks
            $0.scheduleOperations = schedules
            $0.pullRequestOperations = prs
            $0.serviceCommandRecovery = ServiceCommandRecoveryClient(observe: { await services.commandResolutions() })
            $0.nodeOperations = nodes
            $0.newWorkOperations = newWork
            $0.serviceTasksClient = ServiceTasksClient(observe: { await services.taskSnapshots() })
        }
    }
}

private struct ApprovedServiceScene: View {
    let services: CloudAppServices
    let pairing: StoreOf<CloudPairingFeature>
    let notifications: ServiceNotificationRuntime
    @Environment(\.scenePhase) private var phase
    // This scene's context is discarded whenever the approved service identity changes.
    @State private var context = ServiceSceneContext()
    var body: some View {
        Group {
            if let store = context.store { ServiceRootView(store: store, pairing: pairing) }
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

private struct ServiceRootView: View {
    @Bindable var store: StoreOf<ServiceRootFeature>
    let pairing: StoreOf<CloudPairingFeature>
    var body: some View {
        TabView(selection: Binding(get: { store.tab }, set: { store.send(.tabSelected($0)) })) {
            Tab("Waiting for You", systemImage: "tray.full", value: ServiceRootFeature.State.Tab.inbox) {
                NavigationStack { ServiceTriageView(store: store.scope(state: \.triage, action: \.triage)) }
            }.badge(store.triage.triage.queue.count)
            Tab("Tasks", systemImage: "checklist", value: ServiceRootFeature.State.Tab.tasks) {
                ServiceTasksView(store: store.scope(state: \.tasks, action: \.tasks), focusInbox: { store.send(.focusInbox($0)) })
            }
            Tab("Fleet", systemImage: "circle.hexagongrid", value: ServiceRootFeature.State.Tab.fleet) {
                ServiceFleetView(store: store.scope(state: \.fleet, action: \.fleet))
            }
            Tab("New", systemImage: "plus.bubble", value: ServiceRootFeature.State.Tab.newRequest) {
                NavigationStack { ServiceNewRequestView(store: store.scope(state: \.newRequest, action: \.newRequest)) }
            }
            Tab("Account", systemImage: "person.crop.circle", value: ServiceRootFeature.State.Tab.account) {
                NavigationStack { CloudPairingScreen(store: pairing) }
            }
        }
        .task { await store.send(.appeared).finish() }
        .onDisappear { store.send(.disappeared) }
        .sheet(item: $store.scope(state: \.pullRequest, action: \.pullRequest)) { child in
            NavigationStack { ServicePullRequestView(store: child) }
        }
    }
}
