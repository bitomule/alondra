import AlondraCloudPairingFeature
import ComposableArchitecture
import SwiftUI

/// The Mac is an account client, including when the executor runs on this machine.
struct MacServicePairingScreen: View {
    @Bindable var store: StoreOf<CloudPairingFeature>

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                MacServicePairingHeader()
                MacServicePairingStep(store: store)
                if let error = store.error {
                    Text(error)
                        .foregroundStyle(.red)
                        .textSelection(.enabled)
                        .accessibilityIdentifier("pairing.error")
                }
            }
            .padding(32)
            .frame(maxWidth: 580, alignment: .leading)
            .frame(maxWidth: .infinity)
        }
        .navigationTitle("Connect Alondra")
        .task { store.send(.appeared) }
    }
}

private struct MacServicePairingHeader: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Image(systemName: "desktopcomputer.and.arrow.down")
                .font(.system(size: 40))
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
            Text("Connect this Mac").font(.largeTitle.bold())
            Text("Sign in with the account used for Alondra setup, then paste the pairing invitation from the Mac running your work.")
                .foregroundStyle(.secondary)
        }
    }
}

private struct MacServicePairingStep: View {
    @Bindable var store: StoreOf<CloudPairingFeature>

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            switch store.phase {
            case .restoring:
                ProgressView("Restoring your connection…")
                Button("Cancel") { store.send(.cancel) }
            case .loggingIn:
                ProgressView("Complete sign in in your browser…")
                Button("Cancel") { store.send(.cancel) }
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
            case .scanning:
                Text("Copy the full pairing invitation shown by Alondra setup.")
                TextField("Pairing QR contents", text: $store.payload, axis: .vertical)
                    .lineLimit(3...8)
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled()
                    .accessibilityIdentifier("pairing.payload")
                HStack {
                    Button("Connect") { store.send(.submit) }
                        .buttonStyle(.borderedProminent)
                        .disabled(store.payload.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                        .accessibilityIdentifier("pairing.submit")
                    Button("Sign out") { store.send(.logout) }
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
            case .approved:
                Label("Mac connected", systemImage: "checkmark.shield")
                Button("Sign out") { store.send(.logout) }
            }
        }
    }
}
