import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// What the sign-in and pairing screens share: the brand line, a large title, the three steps,
/// a card for the step at hand, and the error that stays until the next attempt.
struct PairingLayout<Title: View, Subtitle: View, Content: View, Footer: View>: View {
    let store: StoreOf<SignInFeature>
    let glow: Color
    let step: Int
    @ViewBuilder let title: Title
    @ViewBuilder let subtitle: Subtitle
    @ViewBuilder let content: Content
    @ViewBuilder let footer: Footer

    init(
        store: StoreOf<SignInFeature>, glow: Color, step: Int,
        @ViewBuilder title: () -> Title, @ViewBuilder subtitle: () -> Subtitle,
        @ViewBuilder content: () -> Content, @ViewBuilder footer: () -> Footer = { EmptyView() }
    ) {
        self.store = store
        self.glow = glow
        self.step = step
        self.title = title()
        self.subtitle = subtitle()
        self.content = content()
        self.footer = footer()
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 28) {
                HStack(spacing: 10) {
                    Image(systemName: "sunrise.fill").foregroundStyle(Palette.dawn)
                    Text("ALONDRA").font(.caption.weight(.bold)).tracking(3)
                    Spacer()
                    Image(systemName: "lock.shield").foregroundStyle(.secondary)
                }
                .padding(.top, 18)
                .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 12) {
                    title
                        .font(.display(.largeTitle, weight: .bold))
                        .foregroundStyle(Palette.ink)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityAddTraits(.isHeader)
                    subtitle
                        .font(.body)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                steps
                VStack(alignment: .leading, spacing: 20) { content }
                    .padding(24)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Palette.card, in: .rect(cornerRadius: 28, style: .continuous))
                    .overlay { RoundedRectangle(cornerRadius: 28, style: .continuous).strokeBorder(Palette.hairline) }
                    .shadow(color: .black.opacity(0.12), radius: 24, y: 12)
                if let error = store.error {
                    Label(error, systemImage: "exclamationmark.circle")
                        .font(.callout)
                        .foregroundStyle(Palette.rose)
                        .accessibilityIdentifier("pairing.error")
                }
                footer
                Label("Your work stays encrypted between your devices.", systemImage: "lock.fill")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 24)
            .padding(.bottom, 32)
            .frame(maxWidth: 540)
            .frame(maxWidth: .infinity)
        }
        .scrollDismissesKeyboard(.interactively)
        .background(DawnBackground(glow: glow))
        .accessibilityIdentifier("pairing.screen")
        .toolbar(store.additionalMac ? .visible : .hidden, for: .navigationBar)
        .task { store.send(.appeared) }
    }

    private var steps: some View {
        HStack(spacing: 8) {
            ForEach(Array([LocalizedStringKey("Account"), "Your Mac", "Confirm"].enumerated()), id: \.offset) { index, label in
                VStack(spacing: 8) {
                    Image(systemName: index < step ? "checkmark.circle.fill" : "\(index + 1).circle.fill")
                    Text(label).font(.caption.weight(.medium)).lineLimit(1)
                }
                .foregroundStyle(index == step ? Palette.ink : Color.secondary)
                .frame(maxWidth: .infinity)
            }
        }
        .accessibilityElement(children: .combine)
    }
}
