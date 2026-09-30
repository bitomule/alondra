import AlondraShared
import AlondraAttachmentsFeature
import AlondraDemoWorkspace
import AlondraWorkspaceFeature
import AlondraFleetFeature
import AlondraNewWorkFeature
import AlondraAppComposition
import AlondraCloudPairingFeature
import AlondraConnectionFeature
import AlondraSync
import AlondraPullRequestsFeature
import AlondraQuestionsFeature
import AlondraTasksFeature
import AlondraTriageFeature
import AlondraHomeFeature
import AlondraViewsDependency
import ComposableArchitecture
import Observation
import SwiftUI

struct ServiceAppScene: View {
    let owner: CloudPairingOwner
    let notifications: ServiceNotificationRuntime
    @State private var demo = Store(initialState: DemoModeFeature.State()) { DemoModeFeature() }
    var body: some View {
        Group {
        if demo.sessionID != nil {
            DemoWorkspaceScreen(mode: demo) { store in
                ServiceRootView(store: store, accountContent: DemoAccountView())
            }
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
                        if owner.store.phase == .signedOut { DemoEntryButton(store: demo).padding() }
                    }
            }
        }
        }.task { await owner.store.send(.appeared).finish() }
    }
}

/// Dependency assembly belongs to scene composition, outside rendering and reducers.
@MainActor @Observable
private final class ServiceSceneContext {
    let settings = ServiceSettingsContext()
    private(set) var store: StoreOf<ServiceRootFeature>?

    func consumeNotification(_ notifications: ServiceNotificationRuntime, services: CloudAppServices, active: Bool) {
        guard active, let store, let route = notifications.consumeRoute(for: services) else { return }
        store.send(.focusInbox(route.itemID))
    }

    func connect(_ services: CloudAppServices) async {
        let viewsFeed = await services.viewsFeed
        let viewActionSender = await services.viewActionSender
        guard !Task.isCancelled else { return }
        store = Store(initialState: ServiceRootFeature.State(usesViews: true)) { ServiceRootFeature() } withDependencies: {
            $0.viewsClient = ViewsClient.live(session: viewsFeed, sender: viewActionSender)
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
    // This scene's context is discarded whenever the approved service identity changes.
    @State private var context = ServiceSceneContext()
    var body: some View {
        Group {
            if let store = context.store {
                ServiceRootView(store: store, accountContent: ServiceSettingsScene(services: services, owner: owner, notifications: notifications, context: context.settings), usesViews: true)
                    .connectionLostOverlay(store: store, retry: retry, signIn: { owner.store.send(.reauthenticate) }, usesViews: true)
            }
            else { ProgressView("Opening workspace…") }
        }
        .retriesConnection(networkChanges: { NetworkReachability.shared.changes() }) { retry() }
        .onChange(of: owner.store.restorationInFlight) { wasSigningIn, signingIn in
            if wasSigningIn && !signingIn { retry() }
        }
        .safeAreaInset(edge: .bottom) {
            if let error = notifications.statusError { Text(error).font(.footnote).padding().accessibilityLabel("Notifications: " + error) }
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

    private func retry() {
        Task { await services.retryViewsConnection() }
    }
}

struct ServiceRootView<AccountContent: View>: View {
    @Bindable var store: StoreOf<ServiceRootFeature>
    let accountContent: AccountContent
    var usesViews = false
    @Namespace private var triageZoom
    var body: some View {
        let presentedPRID = store.pullRequestID
        TabView(selection: Binding<ServiceRootFeature.State.Tab>(get: { store.tab }, set: { store.send(.tabSelected($0)) })) {
            Tab("Waiting for You", systemImage: "tray.full", value: ServiceRootFeature.State.Tab.inbox) {
                NavigationStack {
                    ServiceHomeView(store: store.scope(state: \.home, action: \.home), zoom: triageZoom,
                        openReview: { key in
                            if usesViews { store.send(.openServerView(key)) }
                            else if key.hasPrefix("pull_request:") {
                                store.send(.openLegacyReview(String(key.dropFirst("pull_request:".count))))
                            } else if key.hasPrefix("card:") {
                                store.send(.focusInbox("interaction:" + String(key.dropFirst("card:".count))))
                            }
                        })
                        .navigationDestination(isPresented: Binding(get: { !usesViews && store.triage.documentID != nil }, set: {
                            if !$0 { store.send(.triage(.documentDismissed)) }
                        })) {
                            if let id = store.triage.documentID,
                               let child = store.scope(state: \.triage.documents, action: \.triage.documents).first(where: { $0.subject.id == id }) {
                                ServiceDocumentView(store: child, markRead: {
                                    store.send(.triage(.triage(.hide(child.subject.id))))
                                    store.send(.triage(.documentDismissed))
                                })
                                .navigationTransition(.zoom(sourceID: child.subject.id, in: triageZoom))
                            }
                        }
                        .navigationDestination(isPresented: Binding(get: { !usesViews && store.pullRequestID != nil }, set: { if !$0, let presentedPRID { store.send(.pullRequestDismissed(presentedPRID)) } })) {
                            if let id = store.pullRequestID,
                                      let child = store.scope(state: \.pullRequests, action: \.pullRequests).first(where: { $0.id == id }) {
                                ServicePullRequestView(store: child)
                                    .navigationTransition(.zoom(sourceID: "p\(child.pr.id)", in: triageZoom))
                            }
                        }
                        .navigationDestination(isPresented: Binding(get: { store.serverDetailPresented }, set: {
                            if !$0 { store.send(.serverDetailDismissed) }
                        })) {
                            ViewsScreenView(store: store.scope(state: \.serverReview, action: \.serverReview))
                        }
                        .toolbar { ToolbarItem(placement: .topBarTrailing) {
                            Button("Settings", systemImage: "gearshape") { store.send(.settingsPresented(true)) }
                        } }
                }
            }.badge(store.home.badge)
            Tab("Tasks", systemImage: "checklist", value: ServiceRootFeature.State.Tab.tasks) {
                if usesViews {
                    NavigationStack {
                        ViewsScreenView(store: store.scope(state: \.serverTasks, action: \.serverTasks))
                            .toolbar { ToolbarItem(placement: .topBarTrailing) {
                                Button("Settings", systemImage: "gearshape") { store.send(.settingsPresented(true)) }
                            } }
                    }
                } else {
                    ServiceTasksView(store: store.scope(state: \.tasks, action: \.tasks), focusInbox: { store.send(.focusInbox($0)) }, showSettings: { store.send(.settingsPresented(true)) })
                }
            }
            Tab("Fleet", systemImage: "circle.hexagongrid", value: ServiceRootFeature.State.Tab.fleet) {
                if usesViews {
                    NavigationStack { ViewsScreenView(store: store.scope(state: \.serverFleet, action: \.serverFleet)) }
                } else {
                    ServiceFleetView(store: store.scope(state: \.fleet, action: \.fleet), focusInbox: { store.send(.focusInbox($0)) })
                }
            }
            Tab("New", systemImage: "plus.bubble", value: ServiceRootFeature.State.Tab.newRequest) {
                if usesViews {
                    NavigationStack { ViewsScreenView(store: store.scope(state: \.serverNewRequest, action: \.serverNewRequest)) }
                } else {
                    NavigationStack { ServiceNewRequestView(store: store.scope(state: \.newRequest, action: \.newRequest)) }
                }
            }
        }
        .overlay(alignment: .bottom) { ServiceNoticeToast(store: store) }
        .closeRefusalAlert(Binding(get: { store.closeRefusal }, set: { if $0 == nil { store.send(.dismissCloseRefusal) } }))
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
    }

}
