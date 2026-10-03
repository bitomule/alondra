import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The sheet an action opens when it needs words first, dressed like the app's other writing
/// screens: the dawn background, the title in the display face, and the close and send buttons
/// as glass symbols in its toolbar. The background is opaque so the screen behind it does not
/// smear through the half-height sheet.
public struct ActionComposerSheet<Content: View>: View {
    /// What the trailing button does: send words on, or confirm a form.
    public enum Submit {
        case send
        case confirm(String)

        var symbol: String {
            switch self {
            case .send: "arrow.up"
            case .confirm: "checkmark"
            }
        }

        var label: String {
            switch self {
            case .send: String(localized: "Send")
            case .confirm(let label): label
            }
        }
    }

    let store: StoreOf<ActionFeature>
    let title: String
    let submit: Submit
    let tint: Color
    let content: Content

    public init(store: StoreOf<ActionFeature>, title: String, submit: Submit = .send,
                tint: Color = Palette.dawn, @ViewBuilder content: () -> Content) {
        self.store = store
        self.title = title
        self.submit = submit
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
            .background(DawnBackground(glow: tint))
            .navigationTitle(Text(verbatim: title))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    Text(verbatim: title)
                        .font(.display(.headline))
                        .foregroundStyle(Palette.ink)
                        .lineLimit(1)
                }
                ToolbarItem(placement: .cancellationAction) {
                    Button(role: .cancel) { store.send(.dismiss) } label: {
                        Image(systemName: "xmark")
                    }
                    .accessibilityLabel("Cancel")
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button { store.send(.submit) } label: {
                        Image(systemName: submit.symbol)
                    }
                    .buttonStyle(.glassProminent)
                    .tint(tint)
                    .disabled(!store.canSubmit)
                    .accessibilityLabel(Text(verbatim: submit.label))
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationBackground(Palette.night)
    }
}
