import AlondraContract
import AlondraDesign
import ComposableArchitecture
import SwiftUI

public struct RequestChoicePicker: View {
    @Bindable var store: StoreOf<RequestOptionsFeature>
    public let fieldID: String
    @Environment(\.dismiss) private var dismiss

    public init(store: StoreOf<RequestOptionsFeature>, fieldID: String) { self.store = store; self.fieldID = fieldID }

    public var body: some View {
        let groups = store.state.choiceGroups(fieldID)
        List {
            ForEach(groups) { group in
                Section {
                    ForEach(group.items) { item in
                        Button {
                            store.send(.choose(fieldID, item.id))
                            dismiss()
                        } label: {
                            HStack(alignment: .top) {
                                VStack(alignment: .leading, spacing: 3) {
                                    Text(verbatim: item.label).foregroundStyle(Palette.ink)
                                    if let description = store.state.unavailableReason(item) ?? item.description {
                                        Text(verbatim: description).font(.caption).foregroundStyle(.secondary)
                                    }
                                }
                                Spacer(minLength: 8)
                                if store.state.selected(fieldID) == item.id {
                                    Image(systemName: "checkmark").foregroundStyle(Palette.ember).accessibilityHidden(true)
                                }
                            }
                        }
                        .disabled(!store.state.isAvailable(item))
                        .accessibilityAddTraits(store.state.selected(fieldID) == item.id ? .isSelected : [])
                        .accessibilityIdentifier("request.option.\(fieldID).\(item.id)")
                    }
                } header: {
                    if !group.title.isEmpty { Text(verbatim: group.title) }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(DawnBackground(glow: Palette.ember))
        .navigationTitle(Text(verbatim: store.state.field(fieldID)?.label ?? ""))
        .searchable(text: $store.choiceQuery)
        .overlay { if groups.isEmpty { ContentUnavailableView.search(text: store.choiceQuery) } }
        .onAppear { store.choiceQuery = "" }
    }
}
