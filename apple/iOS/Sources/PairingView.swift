import AVFoundation
import AlondraShared
import SwiftUI
import VisionKit

struct PairingView: View {
    var showsClose = false

    @Environment(DevicePairing.self) private var pairing
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL
    @AppStorage(ServerSetting.key) private var serverURL = ServerSetting.defaultURL

    @State private var camera = CameraAccess.unknown
    @State private var url = ""
    @State private var code = ""
    @State private var phase = Phase.idle
    @FocusState private var focused: Field?

    private enum Field { case url, code }

    private enum Phase: Equatable {
        case idle, pairing, paired, failed(String)
    }

    private enum CameraAccess {
        case unknown, ready, denied, unsupported
    }

    private var manualInvite: PairingInvite? { PairingInvite(url: url, code: code) }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 28) {
                intro
                viewfinder
                manualEntry
            }
            .padding(.horizontal, 20)
            .padding(.top, 8)
            .padding(.bottom, 40)
        }
        .scrollDismissesKeyboard(.interactively)
        .background(DawnBackground(glow: phase == .paired ? Palette.sage : Palette.dawn))
        .navigationTitle("Vincular con el Mac")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if showsClose, phase != .pairing {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cerrar", systemImage: "xmark") { dismiss() }
                }
            }
        }
        .onAppear {
            if url.isEmpty, let current = AlondraClient.serverURL(serverURL), !AlondraClient.isLoopback(current) {
                url = current.absoluteString
            }
        }
        .task { await prepareCamera() }
        .sensoryFeedback(trigger: phase) { _, new in
            switch new {
            case .paired: .success
            case .failed: .error
            case .pairing: .impact(weight: .medium)
            case .idle: nil
            }
        }
        .animation(.spring(duration: 0.45, bounce: 0.25), value: phase)
    }

    private var intro: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Tu iPhone, de confianza")
                .font(.display(.title, weight: .bold))
                .foregroundStyle(Palette.ink)
            Text("En el Mac, ejecuta \(Text("alondra pair").font(.mono(.subheadline, weight: .semibold)).foregroundStyle(Palette.dawn)) y apunta la cámara al código. El código sirve una vez y caduca en diez minutos.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
        }
    }

    private var viewfinder: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 32, style: .continuous)
                .fill(Palette.card)
            switch camera {
            case .ready where phase == .idle || isFailed:
                QRScanner { payload in
                    guard phase == .idle || isFailed, let invite = PairingInvite(qrPayload: payload) else { return }
                    Task { await pair(invite) }
                }
                .clipShape(.rect(cornerRadius: 32, style: .continuous))
                .transition(.opacity)
            default:
                placeholder
            }
            ViewfinderCorners()
                .stroke(cornerTint, style: StrokeStyle(lineWidth: 4, lineCap: .round, lineJoin: .round))
                .padding(28)
                .allowsHitTesting(false)
            if phase == .pairing || phase == .paired {
                statusOverlay
                    .transition(.scale(scale: 0.8).combined(with: .opacity))
            }
        }
        .aspectRatio(camera == .unsupported ? 1.7 : 1, contentMode: .fit)
        .overlay {
            RoundedRectangle(cornerRadius: 32, style: .continuous)
                .strokeBorder(Palette.hairline, lineWidth: 1)
        }
        .shadow(color: .black.opacity(0.12), radius: 24, y: 12)
    }

    private var isFailed: Bool {
        if case .failed = phase { return true }
        return false
    }

    private var cornerTint: Color {
        switch phase {
        case .paired: Palette.sage
        case .failed: Palette.rose
        default: Palette.dawn
        }
    }

    @ViewBuilder
    private var placeholder: some View {
        VStack(spacing: 14) {
            Image(systemName: camera == .denied ? "video.slash" : "qrcode.viewfinder")
                .font(.system(size: 52, weight: .light))
                .foregroundStyle(Palette.dawn.opacity(0.85))
                .symbolEffect(.breathe, options: .repeating, isActive: camera == .unknown)
            Text(placeholderText)
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            if camera == .denied {
                Button("Abrir Ajustes", systemImage: "gearshape") {
                    if let settings = URL(string: UIApplication.openSettingsURLString) { openURL(settings) }
                }
                .buttonStyle(.glass)
            }
        }
        .padding(48)
        .opacity(phase == .pairing || phase == .paired ? 0.2 : 1)
    }

    private var placeholderText: String {
        switch camera {
        case .unknown: "Preparando la cámara…"
        case .denied: "Alondra no tiene permiso para usar la cámara."
        case .unsupported: "Este dispositivo no puede escanear códigos. Escribe los datos abajo."
        case .ready: ""
        }
    }

    private var statusOverlay: some View {
        VStack(spacing: 12) {
            if phase == .paired {
                Image(systemName: "checkmark.circle.fill")
                    .font(.system(size: 56, weight: .semibold))
                    .foregroundStyle(Palette.sage)
                    .symbolEffect(.bounce, value: phase)
                Text("Vinculado")
                    .font(.display(.title3, weight: .bold))
            } else {
                ProgressView()
                    .controlSize(.large)
                    .tint(Palette.dawn)
                Text("Vinculando…")
                    .font(.headline)
            }
        }
        .foregroundStyle(Palette.ink)
        .padding(28)
        .glassEffect(.regular, in: .rect(cornerRadius: 24))
    }

    private var manualEntry: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("O escríbelo a mano")
                .font(.headline)
                .foregroundStyle(Palette.ink)
            VStack(spacing: 0) {
                field("Dirección", text: $url, prompt: "http://192.168.1.20:47800", field: .url)
                    .keyboardType(.URL)
                    .textContentType(.URL)
                    .submitLabel(.next)
                    .onSubmit { focused = .code }
                Divider().padding(.leading, 16)
                field("Código", text: $code, prompt: "Código de vinculación", field: .code)
                    .submitLabel(.go)
                    .onSubmit { submitManual() }
            }
            .background(Palette.card, in: .rect(cornerRadius: 18, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 18, style: .continuous)
                    .strokeBorder(Palette.hairline, lineWidth: 1)
            }
            if case .failed(let reason) = phase {
                Label(reason, systemImage: "exclamationmark.triangle.fill")
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Palette.rose)
                    .transition(.move(edge: .top).combined(with: .opacity))
            }
            Button(action: submitManual) {
                Text("Vincular")
                    .font(.headline)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 6)
            }
            .buttonStyle(.glassProminent)
            .controlSize(.large)
            .disabled(manualInvite == nil || phase == .pairing || phase == .paired)
        }
    }

    private func field(_ label: String, text: Binding<String>, prompt: String, field: Field) -> some View {
        HStack(spacing: 12) {
            Text(label)
                .font(.subheadline.weight(.medium))
                .foregroundStyle(.secondary)
                .frame(width: 84, alignment: .leading)
            TextField(label, text: text, prompt: Text(prompt))
                .font(.mono(.body))
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .focused($focused, equals: field)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 14)
    }

    private func submitManual() {
        guard let invite = manualInvite else { return }
        focused = nil
        Task { await pair(invite) }
    }

    private func pair(_ invite: PairingInvite) async {
        guard phase != .pairing, phase != .paired else { return }
        phase = .pairing
        do {
            try await pairing.pair(invite)
            phase = .paired
            try? await Task.sleep(for: .seconds(1.1))
            dismiss()
        } catch AlondraError.unauthorized {
            phase = .failed("El Mac no aceptó el código. Pide uno nuevo con alondra pair.")
        } catch {
            phase = .failed(error.friendlyDescription)
        }
    }

    private func prepareCamera() async {
        guard DataScannerViewController.isSupported else {
            camera = .unsupported
            return
        }
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            camera = .ready
        case .notDetermined:
            camera = await AVCaptureDevice.requestAccess(for: .video) ? .ready : .denied
        default:
            camera = .denied
        }
    }
}

private struct ViewfinderCorners: Shape {
    func path(in rect: CGRect) -> Path {
        let length = min(rect.width, rect.height) * 0.16
        let radius: CGFloat = 14
        var path = Path()
        for corner in 0..<4 {
            let x = corner % 2 == 0 ? rect.minX : rect.maxX
            let y = corner < 2 ? rect.minY : rect.maxY
            let dx: CGFloat = corner % 2 == 0 ? 1 : -1
            let dy: CGFloat = corner < 2 ? 1 : -1
            path.move(to: CGPoint(x: x, y: y + dy * length))
            path.addLine(to: CGPoint(x: x, y: y + dy * radius))
            path.addQuadCurve(to: CGPoint(x: x + dx * radius, y: y), control: CGPoint(x: x, y: y))
            path.addLine(to: CGPoint(x: x + dx * length, y: y))
        }
        return path
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
