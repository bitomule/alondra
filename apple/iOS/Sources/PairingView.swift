import AlondraShared
import AlondraCloudPairingFeature
import AlondraAppComposition
import ComposableArchitecture
import SwiftUI
import VisionKit

struct PairingView: View {
    var showsClose = false
    @Environment(CloudPairingOwner.self) private var owner
    var body: some View { CloudPairingScreen(store: owner.store) }
}

struct CloudPairingScreen: View {
    @Bindable var store: StoreOf<CloudPairingFeature>

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
                VStack(alignment: .leading, spacing: 12) {
                    Text(title).font(.display(.largeTitle)).foregroundStyle(Palette.ink)
                        .fixedSize(horizontal: false, vertical: true)
                    Text(subtitle).font(.body).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                steps
                VStack(alignment: .leading, spacing: 20) { content }
                    .padding(24).frame(maxWidth: .infinity, alignment: .leading)
                    .background(Palette.card, in: .rect(cornerRadius: 24))
                    .overlay { RoundedRectangle(cornerRadius: 24).stroke(Palette.hairline) }
                if let error = store.error {
                    Label(error, systemImage: "exclamationmark.circle")
                        .font(.callout).foregroundStyle(Palette.rose)
                        .accessibilityIdentifier("pairing.error")
                }
                Label("Your work stays encrypted between your devices.", systemImage: "lock.fill")
                    .font(.footnote).foregroundStyle(.secondary)
            }
            .padding(.horizontal, 24).padding(.bottom, 32)
            .frame(maxWidth: 540).frame(maxWidth: .infinity)
        }
        .background(DawnBackground())
        .toolbar(.hidden, for: .navigationBar)
        .task { store.send(.appeared) }
    }

    private var title: LocalizedStringKey {
        switch store.phase {
        case .scanning, .submitting: "Bring your Mac along."
        case .pending: "One last check."
        case .approved: "You're connected."
        default: "Your work.\nWithin reach."
        }
    }
    private var subtitle: LocalizedStringKey {
        switch store.phase {
        case .scanning, .submitting: "Connect to the Mac running Alondra to see what needs you, wherever you are."
        case .pending: "Confirm this connection on your Mac. Only devices you approve can access your work."
        case .approved: "Your inbox, tasks and conversations are ready."
        default: "Keep up with your agents, answer a question, and help your work move forward."
        }
    }
    private var step: Int {
        switch store.phase {
        case .scanning, .submitting: 1
        case .pending, .approved: 2
        default: 0
        }
    }
    private var steps: some View {
        HStack(spacing: 8) {
            ForEach(Array([String(localized: "Account"), String(localized: "Your Mac"), String(localized: "Confirm")].enumerated()), id: \.offset) { index, label in
                HStack(spacing: 6) {
                    Image(systemName: index < step ? "checkmark.circle.fill" : "\(index + 1).circle.fill")
                    Text(label).font(.caption.weight(.medium))
                }.foregroundStyle(index == step ? Palette.ink : .secondary)
                if index < 2 { Rectangle().fill(Palette.hairline).frame(height: 1) }
            }
        }.accessibilityElement(children: .combine)
    }
    @ViewBuilder private var content: some View {
        switch store.phase {
        case .signedOut:
            Label("Start with your account", systemImage: "person.crop.circle").font(.headline)
            Text("Use the same account on your iPhone and Mac.").foregroundStyle(.secondary)
            primary("Sign in", symbol: "arrow.right", id: "pairing.signIn") { store.send(.login) }
        case .restoring, .loggingIn, .submitting, .signingOut:
            ProgressView(store.phase == .loggingIn ? "Finish signing in in your browser…" : "Connecting securely…")
                .frame(maxWidth: .infinity).padding(.vertical, 24)
            if store.phase != .signingOut { Button("Cancel") { store.send(.cancel) }.frame(maxWidth: .infinity) }
        case .scanning:
            Label("Open Alondra on your Mac", systemImage: "laptopcomputer").font(.headline)
            Text("Run this command in Terminal to show your connection code.").foregroundStyle(.secondary)
            Text("alondra setup").font(.mono(.body)).textSelection(.enabled)
                .padding(14).frame(maxWidth: .infinity, alignment: .leading)
                .background(Palette.night, in: .rect(cornerRadius: 12))
            if store.cameraReady {
                QRScanner { store.send(.scanned($0)) }.frame(height: 260)
                    .clipShape(.rect(cornerRadius: 16))
                Text("Point your camera at the QR code on your Mac.").font(.footnote).foregroundStyle(.secondary)
            } else {
                primary("Scan Mac code", symbol: "qrcode.viewfinder", id: "pairing.scan") { store.send(.camera) }
            }
            DisclosureGroup("Connect without a camera") {
                VStack(alignment: .leading, spacing: 12) {
                    Text("Copy the invitation from your Mac and paste it here.").font(.footnote).foregroundStyle(.secondary)
                    TextField("Connection invitation", text: $store.payload, axis: .vertical)
                        .lineLimit(2...4).textInputAutocapitalization(.never).autocorrectionDisabled()
                        .padding(12).background(Palette.night, in: .rect(cornerRadius: 12))
                        .accessibilityIdentifier("pairing.payload")
                    Button("Connect to Mac") { store.send(.submit) }.disabled(store.payload.isEmpty)
                        .accessibilityIdentifier("pairing.submit")
                }.padding(.top, 12)
            }.font(.callout)
            Button("Use another account") { store.send(.logout) }.font(.footnote)
        case .pending(let code):
            Label("Confirm on your Mac", systemImage: "checkmark.shield").font(.headline)
            Text("Enter this code in Alondra setup, then approve your iPhone.").foregroundStyle(.secondary)
            Text(verbatim: code).font(.system(.largeTitle, design: .monospaced, weight: .semibold))
                .tracking(2).textSelection(.enabled).frame(maxWidth: .infinity).padding(.vertical, 18)
                .background(Palette.night, in: .rect(cornerRadius: 16))
                .accessibilityIdentifier("pairing.comparisonCode")
            ProgressView("Waiting for your Mac…").font(.callout)
            Button("Start again") { store.send(.cancel) }.font(.callout)
        case .approved:
            Label("Mac connected", systemImage: "checkmark.circle.fill").font(.headline).foregroundStyle(Palette.sage)
            Text("This device has secure access to your workspace.").foregroundStyle(.secondary)
            Button("Sign out", role: .destructive) { store.send(.logout) }
        }
    }
    private func primary(_ label: LocalizedStringKey, symbol: String, id: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack { Text(label); Spacer(); Image(systemName: symbol) }.font(.body.weight(.semibold))
                .padding(.vertical, 8)
        }.buttonStyle(.borderedProminent).tint(Palette.dawn).controlSize(.large)
            .accessibilityIdentifier(id)
    }
}

private struct QRScanner: UIViewControllerRepresentable {
    let onPayload: (String) -> Void

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let scanner = DataScannerViewController(
            recognizedDataTypes: [.barcode(symbologies: [.qr])],
            qualityLevel: .balanced,
            recognizesMultipleItems: false,
            isHighFrameRateTrackingEnabled: false,
            isHighlightingEnabled: false
        )
        scanner.delegate = context.coordinator
        try? scanner.startScanning()
        return scanner
    }

    func updateUIViewController(_ scanner: DataScannerViewController, context: Context) {
        context.coordinator.onPayload = onPayload
    }

    static func dismantleUIViewController(_ scanner: DataScannerViewController, coordinator: Coordinator) {
        scanner.stopScanning()
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(onPayload: onPayload)
    }

    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        var onPayload: (String) -> Void

        init(onPayload: @escaping (String) -> Void) {
            self.onPayload = onPayload
        }

        func dataScanner(_ dataScanner: DataScannerViewController, didAdd addedItems: [RecognizedItem], allItems: [RecognizedItem]) {
            for item in addedItems {
                if case .barcode(let barcode) = item, let payload = barcode.payloadStringValue {
                    onPayload(payload)
                    return
                }
            }
        }
    }
}
