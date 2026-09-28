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
import ComposableArchitecture
import Observation
import SwiftUI

struct MacServiceRoot: View {
    let owner: MacWorkspaceOwner
    @State private var demo = Store(initialState: DemoModeFeature.State()) { DemoModeFeature() }
    @State private var connectingRemote = false
    private var account: CloudPairingOwner { owner.account }
    var body: some View {
        Group {
        if demo.sessionID != nil {
            DemoWorkspaceScreen(mode: demo) { store in
                MacServiceWorkspace(store: store, accountContent: DemoAccountView())
            }
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
                                if account.store.phase == .signedOut { DemoEntryButton(store: demo).padding() }
                            }
                    } else {
                        MacLocalDaemonMissingView { connectingRemote = true }
                            .safeAreaInset(edge: .bottom) { DemoEntryButton(store: demo).padding() }
                    }
                }
            }
        }
        }.task { await owner.run() }
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
        let downloads = await services.attachmentDownloadClient()
        guard !Task.isCancelled else { return }
        store = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() } withDependencies: {
            $0.macConversationSearch = MacConversationSearch(run: { try await services.searchConversations($0) })
            $0.triageService = triage
            $0.questionWorkService = questions
            $0.taskOperations = tasks
            $0.scheduleOperations = schedules
            $0.pullRequestOperations = prs
            $0.deliveryJournal = DeliveryJournalClient(refresh: { try await services.journal().refresh() },
                resolve: { attempt, scope, retry, checked, risk in
                    try await services.journal().resolve(attempt, in: scope, retry: retry, terminalChecked: checked, duplicateRisk: risk)
                })
            $0.serviceCommandRecovery = ServiceCommandRecoveryClient(observe: { await services.commandResolutions() },
                observeFeedback: { await services.reportFeedbackRecoveryEvents() },
                observeDelivery: { await services.deliveryStatuses() },
                acknowledgeDelivery: { try await services.acknowledgeDeliveryWarnings($0, in: $1) })
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
        Task { await services.retryConnection() }
    }
}
