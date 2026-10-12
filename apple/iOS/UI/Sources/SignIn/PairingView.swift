import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// Linking a Mac: scan the code `alondra device invite` shows, or paste the invitation, then confirm the
/// code on the Mac. After a failed attempt the camera stays off and the error stays until the
/// next try.
public struct PairingView: View {
    @Bindable var store: StoreOf<SignInFeature>

    public init(store: StoreOf<SignInFeature>) { self.store = store }

    public var body: some View {
        PairingLayout(store: store, glow: store.phase == .approved ? Palette.sage : Palette.dawn, step: step) {
            title
        } subtitle: {
            subtitle
        } content: {
            content
        }
        .sensoryFeedback(trigger: store.phase) { _, phase in
            switch phase {
            case .approved: .success
            case .pending, .submitting: .impact(weight: .medium)
            default: nil
            }
        }
        .sensoryFeedback(.error, trigger: store.error) { _, error in error != nil }
    }

    private var title: Text {
        switch store.phase {
        case .approved: Text("You're connected.")
        case .pending: Text("One last check.")
        default: Text("Bring your Mac along.")
        }
    }

    private var subtitle: Text {
        switch store.phase {
        case .approved: Text("Your inbox, tasks and conversations are ready.")
        case .pending: Text("Confirm this connection on your Mac. Only devices you approve can access your work.")
        default: Text("Connect to the Mac running Alondra to see what needs you, wherever you are.")
        }
    }

    private var step: Int {
        switch store.phase {
        case .pending, .approved: 2
        default: 1
        }
    }

    @ViewBuilder private var content: some View {
        switch store.phase {
        case .pending(let code):
            Label("Confirm on your Mac", systemImage: "checkmark.shield").font(.headline)
            Text("Enter this code in Alondra setup, then approve your iPhone.").foregroundStyle(.secondary)
            Text(verbatim: code)
                .font(.system(.largeTitle, design: .monospaced, weight: .semibold))
                .tracking(2)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 18)
                .background(Palette.night, in: .rect(cornerRadius: 16))
                .accessibilityIdentifier("pairing.comparisonCode")
            ProgressView("Waiting for your Mac…").font(.callout)
            Button("Start again") { store.send(.cancel) }.font(.callout)
        case .approved:
            Label("Mac connected", systemImage: "checkmark.circle.fill").font(.headline).foregroundStyle(Palette.sage)
            Text("This device has secure access to your workspace.").foregroundStyle(.secondary)
            if !store.additionalMac { Button("Sign out", role: .destructive) { store.send(.logout) } }
        default:
            scanning
        }
    }

    @ViewBuilder private var scanning: some View {
        Label("Open Alondra on your Mac", systemImage: "laptopcomputer").font(.headline)
        Text("Run this command in Terminal to show your connection code.").foregroundStyle(.secondary)
        Text(verbatim: "alondra device invite")
            .font(.mono(.body))
            .textSelection(.enabled)
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Palette.night, in: .rect(cornerRadius: 12))
        if store.phase == .submitting {
            ProgressView("Connecting securely…").frame(maxWidth: .infinity).padding(.vertical, 12)
            Button("Cancel") { store.send(.cancel) }.frame(maxWidth: .infinity)
        } else if store.cameraReady {
            QRScanner { store.send(.scanned($0)) }
                .frame(height: 260)
                .clipShape(.rect(cornerRadius: 24, style: .continuous))
                .overlay {
                    ViewfinderCorners()
                        .stroke(Palette.dawn, style: StrokeStyle(lineWidth: 4, lineCap: .round, lineJoin: .round))
                        .padding(20)
                        .allowsHitTesting(false)
                }
            Text("Point your camera at the QR code on your Mac.").font(.footnote).foregroundStyle(.secondary)
        } else {
            Button { store.send(.camera) } label: {
                Label("Scan Mac code", systemImage: "qrcode.viewfinder").frame(maxWidth: .infinity)
            }
            .buttonStyle(.glassProminent)
            .buttonBorderShape(.capsule)
            .controlSize(.large)
            .tint(Palette.dawn)
            .accessibilityIdentifier("pairing.scan")
        }
        DisclosureGroup("Connect without a camera") {
            VStack(alignment: .leading, spacing: 12) {
                Text("Copy the invitation from your Mac and paste it here.").font(.footnote).foregroundStyle(.secondary)
                TextField("Connection invitation", text: $store.payload, axis: .vertical)
                    .lineLimit(2...4)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .padding(12)
                    .background(Palette.night, in: .rect(cornerRadius: 12))
                    .accessibilityIdentifier("pairing.payload")
                Button("Connect to Mac") { store.send(.submit) }
                    .disabled(store.payload.isEmpty)
                    .accessibilityIdentifier("pairing.submit")
            }
            .padding(.top, 12)
        }
        .font(.callout)
        if store.additionalMac {
            Button("Find a Mac on this network instead") { store.send(.showAccount(false)) }.font(.footnote)
        } else {
            Button("Use another account") { store.send(.logout) }.font(.footnote)
        }
    }
}
