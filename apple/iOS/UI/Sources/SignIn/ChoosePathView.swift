import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The first screen: the two ways to reach the Mac, each with what it brings and what it does not,
/// how to put Alondra on the Mac, and the demo.
struct ChoosePathView: View {
    let store: StoreOf<SignInFeature>
    let demo: (() -> Void)?

    var body: some View {
        PairingLayout(store: store, glow: Palette.dawn, step: 0, steps: [], carded: false) {
            Text("Your work.\nWithin reach.")
        } subtitle: {
            Text("Alondra on your iPhone connects to the Mac where your agents work.")
        } content: {
            VStack(spacing: 16) {
                option(.account, systemImage: "person.crop.circle", title: "With your account",
                       detail: "Alerts on this iPhone, and access from anywhere.", recommended: true)
                option(.localNetwork, systemImage: "wifi", title: "Only on this Wi-Fi",
                       detail: "No account. No alerts on this iPhone: open the app to see what needs you.")
            }
        } footer: {
            VStack(alignment: .leading, spacing: 8) {
                Text("Don't have Alondra on your Mac yet?").font(.footnote.weight(.semibold))
                Text("Run in Terminal:").font(.footnote).foregroundStyle(.secondary)
                InstallCommands().font(.footnote)
            }
            .accessibilityIdentifier("choosePath.install")
            if let demo {
                Button("Explore the demo", systemImage: "sparkles", action: demo)
                    .frame(maxWidth: .infinity)
                    .accessibilityIdentifier("demo.enter")
            }
        }
    }

    private func option(_ path: SignInFeature.Path, systemImage: String, title: LocalizedStringKey,
                        detail: LocalizedStringKey, recommended: Bool = false) -> some View {
        Button { store.send(.choose(path)) } label: {
            HStack(alignment: .top, spacing: 14) {
                Image(systemName: systemImage)
                    .font(.title2)
                    .foregroundStyle(Palette.dawn)
                    .frame(width: 32)
                VStack(alignment: .leading, spacing: 6) {
                    ViewThatFits(in: .horizontal) {
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            Text(title).fixedSize()
                            if recommended { badge }
                            Spacer(minLength: 0)
                        }
                        VStack(alignment: .leading, spacing: 6) {
                            Text(title)
                            if recommended { badge }
                        }
                    }
                    .font(.headline)
                    .foregroundStyle(Palette.ink)
                    Text(detail)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.leading)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                Image(systemName: "chevron.right").foregroundStyle(.tertiary).padding(.top, 4)
            }
            .contentShape(.rect)
            .pairingCard()
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier(path == .account ? "choosePath.account" : "choosePath.localNetwork")
    }

    private var badge: some View {
        Text("Recommended")
            .font(.caption.weight(.semibold))
            .foregroundStyle(Palette.dawn)
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(Palette.dawn.opacity(0.15), in: .capsule)
    }
}
