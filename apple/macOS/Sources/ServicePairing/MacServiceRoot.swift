import AlondraAppComposition
import AlondraCloudPairingFeature
import AlondraShared
import AlondraTriageFeature
import ComposableArchitecture
import SwiftUI

struct MacServiceRoot: View {
    let account: CloudPairingOwner

    var body: some View {
        Group {
            if let services = account.services {
                MacServiceWindow(services: services, account: account)
                    .id(ObjectIdentifier(services))
            } else {
                MacServicePairingScreen(store: account.store)
            }
        }
    }
}

/// Each window owns its inbox selection and observation lifetime. The account owns I/O.
private struct MacServiceWindow: View {
    let services: CloudAppServices
    let account: CloudPairingOwner
    @State private var triage: StoreOf<TriageFeature>?

    var body: some View {
        Group {
            if let triage {
                MacServiceInbox(store: triage)
            } else {
                ProgressView("Opening your inbox…")
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button("Sign out") { account.store.send(.logout) }
                    .accessibilityIdentifier("account.signOut")
            }
        }
        .task {
            if triage == nil {
                let service = await services.triageService()
                guard !Task.isCancelled else { return }
                triage = Store(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
                    $0.triageService = service
                }
            }
        }
    }
}

private struct MacServiceInbox: View {
    @Bindable var store: StoreOf<TriageFeature>

    var body: some View {
        NavigationSplitView {
            List {
                Section("Waiting for you") {
                    ForEach(store.queue) { item in
                        Button { store.send(.review(item.id)) } label: {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(item.title).font(.headline)
                                if let name = item.nodeName {
                                    Text(name).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.vertical, 6)
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
            .navigationSplitViewColumnWidth(min: 240, ideal: 320)
            .overlay {
                if !store.hasSnapshot {
                    ProgressView("Connecting…")
                } else if store.queue.isEmpty {
                    ContentUnavailableView("All caught up", systemImage: "checkmark.circle")
                }
            }
        } detail: {
            if let item = store.presentedItem {
                MacServiceInboxDetail(item: item, pending: store.pendingIDs.contains(item.id)) {
                    store.send(.hide(item.id))
                }
            } else {
                ContentUnavailableView("Select an item", systemImage: "tray")
            }
        }
        .navigationTitle("Alondra")
        .safeAreaInset(edge: .bottom) {
            if let notice = store.notice {
                HStack {
                    Text(notice).textSelection(.enabled)
                    Button("Dismiss") { store.send(.dismissNotice) }
                }
                .padding()
                .background(.regularMaterial)
            }
        }
        .task { store.send(.appeared) }
        .onDisappear { store.send(.disappeared) }
    }
}

private struct MacServiceInboxDetail: View {
    let item: InboxItem
    let pending: Bool
    let markRead: () -> Void

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                Text(item.title).font(.largeTitle.bold())
                if let name = item.nodeName { Text(name).foregroundStyle(.secondary) }
                switch item {
                case .question(let question):
                    if let detail = question.readableDetail { Text(detail).textSelection(.enabled) }
                case .report(let report):
                    if let body = report.body { Text(body).textSelection(.enabled) }
                    Button("Mark read", action: markRead).disabled(pending)
                case .pullRequest:
                    Button("Dismiss", action: markRead).disabled(pending)
                }
            }
            .padding(28)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
