import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// Connects this Mac, through the account, to Alondra running on another Mac.
struct MacServicePairingScreen: View {
    @Bindable var store: StoreOf<SignInFeature>

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                MacServicePairingHeader()
                Group {
                    if showsFindMac { MacFindMacStep(store: store) } else { MacServicePairingStep(store: store) }
                }
                    .padding(28)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(.background, in: .rect(cornerRadius: 20))
                    .overlay { RoundedRectangle(cornerRadius: 20).stroke(.primary.opacity(0.08)) }
                if let error = store.error {
                    Text(error)
                        .foregroundStyle(.red)
                        .textSelection(.enabled)
                        .accessibilityIdentifier("pairing.error")
                }
            }
            .padding(48)
            .frame(maxWidth: 660, alignment: .leading)
            .frame(maxWidth: .infinity)
        }
        .background(Color(nsColor: .windowBackgroundColor))
        .tint(.orange)
        .navigationTitle("Alondra")
        .task { store.send(.appeared) }
    }

    /// Another Mac on this network comes first; the account flow only after the person asks.
    private var showsFindMac: Bool {
        guard !store.showsAccount else { return false }
        switch store.phase {
        case .signedOut: return true
        case .scanning: return store.additionalMac
        case .approved: return store.findMac.step == .approved
        default: return false
        }
    }
}

private struct MacServicePairingHeader: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Label("ALONDRA", systemImage: "sunrise.fill")
                .font(.caption.weight(.bold)).tracking(3).foregroundStyle(.orange)
            Text("A home for your work.")
                .font(.system(.largeTitle, design: .serif, weight: .semibold))
            Text("Connect your account and the Mac where Alondra works. Your questions, tasks and conversations come together here.")
                .font(.title3).foregroundStyle(.secondary)
            Label("Encrypted between your devices", systemImage: "lock.shield")
                .font(.callout).foregroundStyle(.secondary)
        }.padding(.vertical, 20)
    }
}

private struct MacServicePairingStep: View {
    @Bindable var store: StoreOf<SignInFeature>

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            switch store.phase {
            case .restoring:
                ProgressView("Restoring your connection…")
                Button("Cancel") { store.send(.cancel) }
            case .loggingIn:
                ProgressView("Complete sign in in your browser…")
                Button("Cancel") { store.send(.cancel) }
            case .cancelling:
                ProgressView("Cancelling connection…")
            case .signingOut:
                ProgressView("Signing out…")
            case .submitting:
                ProgressView("Checking the pairing invitation…")
                Button("Cancel") { store.send(.cancel) }
            case .signedOut:
                Button("Sign in") { store.send(.login) }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                    .accessibilityIdentifier("pairing.signIn")
                Button("Find a Mac on this network") { store.send(.showAccount(false)) }
            case .scanning:
                Label("Connect your workspace", systemImage: "laptopcomputer").font(.title2.bold())
                Text("On the Mac running Alondra, run this command in Terminal and copy the connection invitation.")
                    .foregroundStyle(.secondary)
                Text(verbatim: "alondra device invite").font(.system(.body, design: .monospaced))
                    .textSelection(.enabled).padding(14).frame(maxWidth: .infinity, alignment: .leading)
                    .background(.quaternary, in: .rect(cornerRadius: 10))
                PasteButton(payloadType: String.self) { values in
                    if let invitation = values.first { store.send(.scanned(invitation)) }
                }.controlSize(.large).accessibilityLabel("Paste connection invitation")
                DisclosureGroup("Enter invitation manually") {
                    VStack(alignment: .leading, spacing: 12) {
                        MultilineTextField(String(localized: "Connection invitation"), text: $store.payload, lines: 3...6)
                            .autocorrectionDisabled()
                            .accessibilityIdentifier("pairing.payload")
                        Button("Connect") { store.send(.submit) }
                            .buttonStyle(.borderedProminent).disabled(store.payload.isEmpty)
                            .accessibilityIdentifier("pairing.submit")
                    }.padding(.top, 10)
                }
                if store.additionalMac {
                    Button("Find a Mac on this network instead") { store.send(.showAccount(false)) }
                } else {
                    Button("Use another account") { store.send(.logout) }
                }
            case .pending(let code):
                Text("Approve this Mac in Alondra setup").font(.headline)
                Text(verbatim: code)
                    .font(.system(size: 36, weight: .bold, design: .monospaced))
                    .textSelection(.enabled)
                    .accessibilityIdentifier("pairing.comparisonCode")
                Text("Enter this code in the setup terminal and approve this Mac. Keep this window open until approval completes.")
                ProgressView("Waiting for approval…")
                Button("Cancel pairing") { store.send(.cancel) }
            case .approved, .alerts:
                Label("Mac connected", systemImage: "checkmark.shield")
                if !store.additionalMac { Button("Sign out") { store.send(.logout) } }
            }
        }
    }
}
