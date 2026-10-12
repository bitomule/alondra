import AlondraContract
import ComposableArchitecture
import SwiftUI

/// «This Mac's settings»: one paired Mac's projects folder and whether it reaches you away from home.
public struct ThisMacSettingsView: View {
    @Bindable var store: StoreOf<MacSettingsFeature>

    public init(store: StoreOf<MacSettingsFeature>) { self.store = store }

    public var body: some View {
        Form {
            if let settings = store.settings {
                projects
                relay(settings.relay)
            } else if let problem = store.problem {
                Section {
                    Label { Text(problem.title) } icon: { Image(systemName: problem.systemImage) }
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("macSettings.problem")
                }
            } else {
                Section { ProgressView().frame(maxWidth: .infinity) }
            }
        }
        .formStyle(.grouped)
        .navigationTitle(Text(verbatim: store.macName))
        .sheet(item: $store.scope(state: \.browser, action: \.browser)) { browser in
            NavigationStack { FolderBrowserView(store: browser) }
        }
        .task { await store.send(.task).finish() }
        .accessibilityIdentifier("macSettings")
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
