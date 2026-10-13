import AlondraContract
import ComposableArchitecture
import SwiftUI

/// Settings' «Permissions allowed always» for the connected Mac: each command its nodes no longer
/// ask about, with a Remove button. Absent while that Mac lists none; the screen showing it starts
/// `.task`.
public struct AllowedRulesSection: View {
    let store: StoreOf<AllowedRulesFeature>

    public init(store: StoreOf<AllowedRulesFeature>) { self.store = store }

    public var body: some View {
        if !store.shown.isEmpty || store.error != nil {
            Section {
                ForEach(store.shown) { rule in
                    HStack(alignment: .firstTextBaseline, spacing: 12) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text((try? AttributedString(markdown: rule.title)) ?? AttributedString(rule.title))
                            if let detail = rule.detail {
                                Text(verbatim: detail).font(.subheadline).foregroundStyle(.secondary)
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        Button(role: .destructive) { store.send(.remove(rule), animation: .default) } label: {
                            Text(verbatim: rule.remove.label)
                        }
                        .buttonStyle(.borderless)
                        .fixedSize()
                        .accessibilityIdentifier("settings.allowedRules.remove.\(rule.id)")
                    }
                }
                if let error = store.error {
                    Text(verbatim: error).font(.footnote).foregroundStyle(.red)
                        .accessibilityIdentifier("settings.allowedRules.error")
                }
            } header: {
                Text("Permissions allowed always")
            } footer: {
                Text("Nodes in each repo run these commands without asking. Remove one and the next call asks again.")
            }
            .accessibilityIdentifier("settings.allowedRules")
        }
    }
}
