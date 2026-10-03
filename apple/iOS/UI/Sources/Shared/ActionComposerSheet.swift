import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The sheet an action opens when it needs words first: titled with what is being done, Cancel
/// and the send button in its toolbar, and an opaque background so the screen behind it does not
/// smear through the half-height sheet.
public struct ActionComposerSheet<Content: View>: View {
    let store: StoreOf<ActionFeature>
    let title: String
    let submitTitle: String
    let tint: Color
    let content: Content

    public init(store: StoreOf<ActionFeature>, title: String, submitTitle: String? = nil,
                tint: Color = Palette.dawn, @ViewBuilder content: () -> Content) {
        self.store = store
        self.title = title
        self.submitTitle = submitTitle ?? String(localized: "Send")
        self.tint = tint
        self.content = content()
    }

    public var body: some View {
        NavigationStack {
            ScrollView {
                content
                    .padding(.horizontal, 20)
                    .padding(.vertical, 12)
            }
            .scrollDismissesKeyboard(.interactively)
            .navigationTitle(Text(verbatim: title))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(role: .cancel) { store.send(.dismiss) }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button { store.send(.submit) } label: { Text(verbatim: submitTitle) }
                        .buttonStyle(.glassProminent)
                        .tint(tint)
                        .disabled(!store.canSubmit)
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationBackground(Palette.night)
    }
}
