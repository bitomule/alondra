import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The first screen: sign in with your account, or look around the app on sample data. Once the
/// account is in, the same screen walks on to linking the Mac.
public struct SignInView: View {
    @Bindable var store: StoreOf<SignInFeature>
    let demo: (() -> Void)?

    public init(store: StoreOf<SignInFeature>, demo: (() -> Void)? = nil) {
        self.store = store
        self.demo = demo
    }

    public var body: some View {
        switch store.phase {
        case .scanning, .submitting, .pending, .approved:
            PairingView(store: store)
        case .restoring, .signedOut, .signingOut, .loggingIn, .cancelling:
            signIn
        }
    }

    private var signIn: some View {
        PairingLayout(store: store, glow: Palette.dawn, step: 0) {
            Text("Your work.\nWithin reach.")
        } subtitle: {
            Text("Keep up with your fleet, answer a question, and help your work move forward.")
        } content: {
            switch store.phase {
            case .signedOut:
                Label("Start with your account", systemImage: "person.crop.circle").font(.headline)
                Text("Use the same account on your iPhone and Mac.").foregroundStyle(.secondary)
                Button { store.send(.login) } label: {
                    Label("Sign in", systemImage: "arrow.right").frame(maxWidth: .infinity)
                }
                .buttonStyle(.glassProminent)
                .buttonBorderShape(.capsule)
                .controlSize(.large)
                .tint(Palette.dawn)
                .accessibilityIdentifier("pairing.signIn")
            case .cancelling:
                ProgressView("Cancelling connection…").frame(maxWidth: .infinity).padding(.vertical, 24)
            default:
                ProgressView(store.phase == .loggingIn ? "Finish signing in in your browser…" : "Connecting securely…")
                    .frame(maxWidth: .infinity).padding(.vertical, 24)
                if store.phase != .signingOut {
                    Button("Cancel") { store.send(.cancel) }.frame(maxWidth: .infinity)
                }
            }
        } footer: {
            if store.phase == .signedOut, let demo {
                Button("Explore the demo", systemImage: "sparkles", action: demo)
                    .buttonStyle(.glass)
                    .controlSize(.large)
                    .frame(maxWidth: .infinity)
                    .accessibilityIdentifier("demo.enter")
            }
        }
    }
}
