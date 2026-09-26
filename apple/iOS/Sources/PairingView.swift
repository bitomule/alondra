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
            VStack(alignment: .leading, spacing: 24) {
                Text("Connect your Mac").font(.largeTitle.bold())
                Text("Run alondra setup on your Mac, then sign in with the same account on your iPhone.")
                switch store.phase {
                case .restoring, .loggingIn, .submitting, .signingOut:
                    ProgressView("Connecting…")
                    Button("Cancel") { store.send(.cancel) }
                case .signedOut:
                    Button("Sign in") { store.send(.login) }.buttonStyle(.borderedProminent)
                        .accessibilityIdentifier("pairing.signIn")
                case .scanning:
                    if store.cameraReady {
                        QRScanner { store.send(.scanned($0)) }.frame(height: 280).clipShape(.rect(cornerRadius: 20))
                    }
                    Button("Enable camera") { store.send(.camera) }
                    Text("Scan the QR code from alondra setup, or paste its full contents below.")
                    TextField("Pairing QR contents", text: $store.payload, axis: .vertical)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                        .accessibilityIdentifier("pairing.payload")
                    Button("Pair with Mac") { store.send(.submit) }.disabled(store.payload.isEmpty)
                    Button("Sign out") { store.send(.logout) }
                case .pending(let code):
                    Text("Enter this code on your Mac")
                    Text(verbatim: code).font(.system(size: 36, weight: .bold, design: .monospaced))
                        .textSelection(.enabled).accessibilityIdentifier("pairing.comparisonCode")
                    Text("After entering the code, approve this iPhone on your Mac. Keep this screen open.")
                    ProgressView("Waiting for approval…")
                    Button("Cancel pairing") { store.send(.cancel) }
                case .approved:
                    Label("Mac connected", systemImage: "checkmark.shield")
                    Button("Sign out") { store.send(.logout) }
                }
                if let error = store.error { Text(error).foregroundStyle(.red).accessibilityIdentifier("pairing.error") }
            }.padding(24)
        }.navigationTitle("Pair with Mac").task { store.send(.appeared) }
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
