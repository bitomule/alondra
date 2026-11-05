import AlondraContract
import ComposableArchitecture
import SwiftUI

/// «This Mac's settings»: one paired Mac's projects folder and whether it reaches you away from home.
public struct ThisMacSettingsView: View {
    @Bindable var store: StoreOf<MacSettingsFeature>

    public init(store: StoreOf<MacSettingsFeature>) { self.store = store }

    public var body: some View {
        Form {
            connection
            if let settings = store.settings {
                projects
                relay(settings.relay)
                if let direct = settings.directConnection {
                    DirectConnectionSection(direct: direct)
                }
            } else if store.problem != nil {
                Section {
                    Text("Its settings can’t be read right now. Alondra keeps trying.")
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("macSettings.problem")
                }
            } else {
                Section { ProgressView().frame(maxWidth: .infinity) }
            }
            if store.canForget { forget }
        }
        .formStyle(.grouped)
        .navigationTitle(Text(verbatim: store.macName))
        .sheet(item: $store.scope(state: \.browser, action: \.browser)) { browser in
            NavigationStack { FolderBrowserView(store: browser) }
        }
        .confirmationDialog("Forget this Mac?", isPresented: Binding(get: { store.confirmForget }, set: { if !$0 { store.send(.cancelForget) } }),
                            titleVisibility: .visible) {
            Button("Forget Mac", role: .destructive) { store.send(.confirmForget) }
            Button("Cancel", role: .cancel) { store.send(.cancelForget) }
        } message: {
            Text("This Mac deletes its keys for the other Mac. To use it again, find it and pair it again.")
        }
        .task { await store.send(.task).finish() }
        .accessibilityIdentifier("macSettings")
    }

    @ViewBuilder private var connection: some View {
        if let status = store.status {
            Section {
                MacStatusLabel(status: status)
            } header: {
                Text("Connection")
            }
        }
    }

    private var forget: some View {
        Section {
            Button("Forget This Mac", systemImage: "trash", role: .destructive) { store.send(.forgetTapped) }
                .accessibilityIdentifier("macSettings.forget")
        }
    }

    private var projects: some View {
        Section {
            ForEach(store.candidates) { candidate in
                Button { store.send(.choose(candidate)) } label: {
                    HStack {
                        Image(systemName: candidate.path == store.projectsRoot ? "largecircle.fill.circle" : "circle")
                            .foregroundStyle(candidate.path == store.projectsRoot ? Color.accentColor : .secondary)
                            .accessibilityHidden(true)
                        Text(verbatim: FolderPath.display(candidate.path)).font(.body.monospaced())
                        Spacer()
                        if let repos = candidate.repos {
                            Text("\(repos) repos").foregroundStyle(.secondary)
                        }
                    }
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(candidate.path == store.projectsRoot ? .isSelected : [])
            }
            Button("Other…") { store.send(.other) }
                .accessibilityIdentifier("macSettings.otherFolder")
            if let error = store.error {
                Text(verbatim: error).font(.footnote).foregroundStyle(.red)
                    .accessibilityIdentifier("macSettings.folderError")
            }
        } header: {
            Text("Projects folder")
        }
    }

    private func relay(_ relay: MacSettings.Relay) -> some View {
        Section {
            LabeledContent("Notifications and access away from home") {
                Text(relay.on ? "On" : "Off").foregroundStyle(relay.on ? Color.green : .secondary)
            }
        } footer: {
            VStack(alignment: .leading, spacing: 6) {
                if !relay.on { Text("Without it you only get updates while the app is open on the same Wi-Fi.") }
                Text("To change it, run `alondra relay` on this Mac.")
            }
        }
    }
}
