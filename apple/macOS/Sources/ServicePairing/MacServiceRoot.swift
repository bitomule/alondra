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

struct MacServiceRoot: View {
    let account: CloudPairingOwner
    var body: some View {
        if let services = account.services {
            MacServiceWindow(services: services, account: account)
                .id(ObjectIdentifier(services))
        } else {
            MacServicePairingScreen(store: account.store)
        }
    }
}

/// Scene dependency assembly; authentication and I/O are process-owned by the account.
@MainActor @Observable
private final class MacServiceWindowContext {
    private(set) var store: StoreOf<ServiceRootFeature>?
    func connect(_ services: CloudAppServices) async {
        guard store == nil else { return }
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

private struct MacServiceWindow: View {
    let services: CloudAppServices
    let account: CloudPairingOwner
    @State private var context = MacServiceWindowContext()
    var body: some View {
        Group {
            if let store = context.store {
                MacServiceWorkspace(store: store, account: account)
            } else {
                ProgressView("Opening workspace…")
            }
        }.task { await context.connect(services) }
    }
}

private struct MacServiceWorkspace: View {
    @Bindable var store: StoreOf<ServiceRootFeature>
    let account: CloudPairingOwner
    var body: some View {
        NavigationSplitView {
            List(selection: Binding<ServiceRootFeature.State.Tab?>(get: { store.tab }, set: {
                if let tab = $0 { store.send(.tabSelected(tab)) }
            })) {
                Label("Waiting for You", systemImage: "tray.full")
                    .badge(store.triage.triage.queue.count)
                    .tag(ServiceRootFeature.State.Tab.inbox)
                Label("Tasks", systemImage: "checklist").tag(ServiceRootFeature.State.Tab.tasks)
                Label("Fleet", systemImage: "circle.hexagongrid").tag(ServiceRootFeature.State.Tab.fleet)
                Label("New Request", systemImage: "plus.bubble").tag(ServiceRootFeature.State.Tab.newRequest)
                Label("Account", systemImage: "person.crop.circle").tag(ServiceRootFeature.State.Tab.account)
            }
            .navigationSplitViewColumnWidth(min: 200, ideal: 240)
        } detail: {
            switch store.tab {
            case .inbox:
                NavigationStack { ServiceTriageView(store: store.scope(state: \.triage, action: \.triage)) }
            case .tasks:
                ServiceTasksView(store: store.scope(state: \.tasks, action: \.tasks), focusInbox: { store.send(.focusInbox($0)) })
            case .fleet:
                ServiceFleetView(store: store.scope(state: \.fleet, action: \.fleet))
            case .newRequest:
                ServiceNewRequestView(store: store.scope(state: \.newRequest, action: \.newRequest))
            case .account:
                MacServicePairingScreen(store: account.store)
            }
        }
        .navigationTitle("Alondra")
        .frame(minWidth: 740, minHeight: 520)
        .task { await store.send(.appeared).finish() }
        .onDisappear { store.send(.disappeared) }
        .sheet(item: $store.scope(state: \.pullRequest, action: \.pullRequest)) { child in
            NavigationStack { ServicePullRequestView(store: child) }.frame(minWidth: 720, minHeight: 560)
        }
    }
}
