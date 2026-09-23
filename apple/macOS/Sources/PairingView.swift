import AlondraShared
import CoreImage
import CoreImage.CIFilterBuiltins
import SwiftUI

struct PairingView: View {
    private let client = AlondraClient.local
    private let fallbackURL = "https://alondra.davidcollado.dev"

    @State private var invite: PairingInvite?
    @State private var issuedAt = Date()
    @State private var devices: [PairedDevice] = []
    @State private var knownIDs: Set<String>?
    @State private var justPaired: PairedDevice?
    @State private var error: String?

    var body: some View {
        VStack(spacing: 0) {
            HStack(alignment: .top, spacing: 28) {
                qrPanel
                instructions
            }
            .padding(28)
            Divider()
            devicesPanel
                .padding(.horizontal, 28)
                .padding(.vertical, 18)
        }
        .frame(width: 640)
        .task { await newInvite() }
        .task { await watchDevices() }
    }

    private var qrPanel: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 18, style: .continuous)
                .fill(.white)
                .frame(width: 236, height: 236)
                .shadow(color: .black.opacity(0.12), radius: 12, y: 4)
            if let justPaired {
                VStack(spacing: 10) {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.system(size: 64))
                        .foregroundStyle(.green)
                        .symbolEffect(.bounce, value: justPaired.id)
                    Text(justPaired.name).font(.headline).foregroundStyle(.black)
                }
                .transition(.scale.combined(with: .opacity))
            } else if let invite, let image = qrImage(invite.payload(fallbackURL: fallbackURL)) {
                Image(nsImage: image)
                    .interpolation(.none)
                    .resizable()
                    .frame(width: 204, height: 204)
                    .transition(.opacity)
            } else {
                ProgressView()
            }
        }
        .animation(.spring(duration: 0.4), value: justPaired?.id)
    }

    private var instructions: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(justPaired == nil ? "Vincula tu iPhone" : "iPhone vinculado").font(.title2.bold())
            if justPaired == nil {
                VStack(alignment: .leading, spacing: 8) {
                    Label("Abre Alondra en el iPhone", systemImage: "1.circle")
                    Label("Ajustes → Vincular con el Mac", systemImage: "2.circle")
                    Label("Apunta la cámara a este código", systemImage: "3.circle")
                }
                .foregroundStyle(.secondary)
                if let invite {
                    TimelineView(.periodic(from: .now, by: 1)) { context in
                        let left = max(0, invite.expiresIn - Int(context.date.timeIntervalSince(issuedAt)))
                        VStack(alignment: .leading, spacing: 6) {
                            Text(left > 0 ? "Caduca en \(left / 60):\(String(format: "%02d", left % 60)) · sirve una vez" : "Caducado")
                                .font(.callout)
                                .foregroundStyle(left > 0 ? Color.secondary : Color.orange)
                            DisclosureGroup("¿Sin cámara?") {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(invite.url ?? fallbackURL).textSelection(.enabled)
                                    Text(invite.code).font(.body.monospaced()).textSelection(.enabled)
                                }
                                .font(.callout)
                                .padding(.top, 4)
                            }
                            .font(.callout)
                        }
                    }
                }
            } else {
                Text("Ya puedes contestar desde el iPhone, también fuera de casa.").foregroundStyle(.secondary)
            }
            if let error { Text(error).font(.callout).foregroundStyle(.red) }
            Spacer(minLength: 0)
            Button(justPaired == nil ? "Generar otro código" : "Vincular otro") {
                Task { await newInvite() }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var devicesPanel: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Dispositivos vinculados").font(.headline)
            if devices.isEmpty {
                Text("Ninguno todavía.").foregroundStyle(.secondary)
            }
            ForEach(devices) { device in
                HStack {
                    Image(systemName: "iphone")
                    VStack(alignment: .leading) {
                        Text(device.name)
                        Text(device.lastSeen.map { "Visto hace \(ago($0))" } ?? "Sin uso todavía")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("Desvincular", role: .destructive) {
                        Task { await revoke(device) }
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func qrImage(_ text: String) -> NSImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(text.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage?.transformed(by: CGAffineTransform(scaleX: 8, y: 8)),
              let cgImage = CIContext().createCGImage(output, from: output.extent) else { return nil }
        return NSImage(cgImage: cgImage, size: NSSize(width: output.extent.width, height: output.extent.height))
    }

    private func newInvite() async {
        do {
            justPaired = nil
            invite = try await client.createPairing()
            issuedAt = Date()
            error = nil
        } catch {
            self.error = "No pude pedir un código: \(error.localizedDescription)"
        }
    }

    private func watchDevices() async {
        while !Task.isCancelled {
            if let list = try? await client.pairedDevices() {
                if let known = knownIDs, let fresh = list.first(where: { !known.contains($0.id) }) {
                    withAnimation { justPaired = fresh }
                }
                knownIDs = Set(list.map(\.id))
                devices = list
            }
            try? await Task.sleep(for: .seconds(2))
        }
    }

    private func revoke(_ device: PairedDevice) async {
        do {
            try await client.revokeDevice(device.id)
            devices.removeAll { $0.id == device.id }
        } catch {
            self.error = error.localizedDescription
        }
    }
}
