import AlondraContract
import ComposableArchitecture
import SwiftUI

/// Settings' rows for the connected Mac, each with the menu its daemon offers to change it, in the
/// daemon's order. A row is absent while that Mac does not offer it; the screen showing them
/// starts `.task`.
public struct MachineSettingsSection: View {
    let store: StoreOf<MachineSettingsFeature>

    public init(store: StoreOf<MachineSettingsFeature>) { self.store = store }

    public var body: some View {
        ForEach(store.settings) { setting in
            Section {
                LabeledContent {
                    Menu {
                        ForEach(setting.action.options, id: \.id) { option in
                            Button { store.send(.choose(setting.kind, option)) } label: {
                                Text(option.label)
                                if let description = option.description { Text(description) }
                            }
                        }
                    } label: {
                        Text("Change")
                    }
                    .disabled(store.sending.contains(setting.kind))
                    .accessibilityIdentifier("settings.\(setting.kind.accessibilityName).menu")
                } label: {
                    Text(setting.title)
                    if let detail = store.state.detail(setting.kind) { Text(detail) }
                }
                if let error = store.errors[setting.kind] {
                    Text(error).font(.footnote).foregroundStyle(.red)
                }
            } footer: {
                footer(setting.kind)
            }
        }
    }

    private func footer(_ kind: MachineSettingKind) -> Text {
        switch kind {
        case .mergeMethod: Text("How the Mac merges a PR when you press its button or it merges on its own. If a repository doesn’t allow this method, it uses the first one the repository allows.")
        case .branchUpdate: Text("How a node brings main into its PR’s branch when the PR has conflicts. With rebase it pushes with --force-with-lease, only to its own task’s branch; if the rebase gets hard, it merges that once and says so.")
        case .nodeLimit: Text("How many nodes work at the same time on the Mac. The rest wait their turn.")
        }
    }
}

private extension MachineSettingKind {
    var accessibilityName: String {
        switch self {
        case .mergeMethod: "mergeMethod"
        case .branchUpdate: "branchUpdate"
        case .nodeLimit: "nodeLimit"
        }
    }
}
