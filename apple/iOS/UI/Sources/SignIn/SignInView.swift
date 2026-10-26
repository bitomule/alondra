import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The first screen: the account or this Wi-Fi, each with what it brings, or a look around the app
/// on sample data. Once the account is in, the same screen walks on to linking the Mac.
public struct SignInView: View {
    @Bindable var store: StoreOf<SignInFeature>
    let demo: (() -> Void)?

    public init(store: StoreOf<SignInFeature>, demo: (() -> Void)? = nil) {
        self.store = store
        self.demo = demo
    }

    public var body: some View {
        if choosesPath {
            ChoosePathView(store: store, demo: demo)
        } else if showsFindMac {
            FindMacView(store: store, demo: demo)
        } else {
            switch store.phase {
            case .scanning, .submitting, .pending, .alerts, .approved:
                PairingView(store: store)
            case .restoring, .signedOut, .signingOut, .loggingIn, .cancelling:
                signIn
            }
        }
    }

    private var choosesPath: Bool {
        store.phase == .signedOut && store.path == nil && !store.additionalMac
    }

    private var showsFindMac: Bool {
        guard !store.showsAccount else { return false }
        switch store.phase {
        case .signedOut: return true
        case .scanning: return store.additionalMac
        case .approved: return store.findMac.step == .approved
        default: return false
        }
    }

    private var signIn: some View {
        PairingLayout(store: store, glow: Palette.dawn, step: 0) {
            Text("Start with your account.")
        } subtitle: {
            Text("Alerts on this iPhone, and your Mac within reach from anywhere.")
        } content: {
            switch store.phase {
            case .signedOut:
                Label("Sign in with Alondra", systemImage: "person.crop.circle").font(.headline)
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
            if store.phase == .signedOut, !store.linksAccount {
                Button("Find a Mac on this network", systemImage: "wifi") { store.send(.showAccount(false)) }
                    .buttonStyle(.glass)
                    .controlSize(.large)
                    .frame(maxWidth: .infinity)
                    .accessibilityIdentifier("pairing.findMac")
            }
        }
    }
}
