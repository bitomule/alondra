import AlondraContract
import ComposableArchitecture
import SwiftUI

/// Settings' "Nodes at once" for the connected Mac: where the limit comes from and the menu the
/// daemon offers to change it. Absent while that Mac does not offer it; the screen showing it
/// starts `.task`.
public struct NodeLimitSection: View {
    let store: StoreOf<NodeLimitFeature>

    public init(store: StoreOf<NodeLimitFeature>) { self.store = store }

    public var body: some View {
        if let setting = store.setting {
            Section {
                LabeledContent {
                    Menu {
                        ForEach(setting.action.options, id: \.id) { option in
                            Button { store.send(.choose(option)) } label: {
                                Text(option.label)
                                if let description = option.description { Text(description) }
                            }
                        }
                    } label: {
                        Text("Change")
                    }
                    .disabled(store.sending)
                    .accessibilityIdentifier("settings.nodeLimit.menu")
                } label: {
                    Text(setting.title)
                    if let detail = store.detail { Text(detail) }
                }
                if let error = store.error {
                    Text(error).font(.footnote).foregroundStyle(.red)
                }
            } footer: {
                Text("How many nodes work at the same time on the Mac. The rest wait their turn.")
            }
        }
    }
}
