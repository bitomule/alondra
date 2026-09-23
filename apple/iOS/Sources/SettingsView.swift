import AlondraShared
import SwiftUI

struct SettingsView: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(DevicePairing.self) private var pairing
    @AppStorage(ServerSetting.key) private var serverURL = ServerSetting.defaultURL
    @State private var draft = ""
    @State private var check: Check = .idle
    @State private var detent = PresentationDetent.medium

    private enum Check: Equatable {
        case idle, checking, ok, failed(String)
    }

    private var candidate: AlondraClient? { AlondraClient(string: draft, device: "iphone", signer: pairing.signer) }

    var body: some View {
        NavigationStack {
            Form {
                PairingSection(detent: $detent)
                NotificationsSection()
                Section {
                    TextField("http://127.0.0.1:47800", text: $draft)
                        .font(.mono(.body))
                        .keyboardType(.URL)
                        .textContentType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .onChange(of: draft) { check = .idle }
                    Button {
                        Task { await test() }
                    } label: {
                        HStack {
                            Text("Probar conexión")
                            Spacer()
                            checkBadge
                        }
                    }
                    .disabled(candidate == nil || check == .checking)
                } header: {
                    Text("Servidor")
                } footer: {
                    Text(candidate == nil && !draft.isEmpty ? "Escribe una dirección http:// o https:// completa." : "Donde escucha alondrad. En el simulador, el Mac es 127.0.0.1.")
                }
                Section {
                    Button("Volver a la dirección local") { draft = ServerSetting.defaultURL }
                        .disabled(draft == ServerSetting.defaultURL)
                }
            }
            .navigationTitle("Ajustes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cerrar", systemImage: "xmark") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Guardar", systemImage: "checkmark") {
                        if let candidate { serverURL = candidate.base.absoluteString }
                        dismiss()
                    }
                    .disabled(candidate == nil)
                }
            }
            .onAppear { draft = serverURL }
            .presentationDetents([.medium, .large], selection: $detent)
            .sensoryFeedback(trigger: check) { _, new in
                switch new {
                case .ok: .success
                case .failed: .error
                default: nil
                }
            }
        }
    }

    @ViewBuilder
    private var checkBadge: some View {
        switch check {
        case .idle: EmptyView()
        case .checking: ProgressView()
        case .ok: Label("Conectado", systemImage: "checkmark.circle.fill").foregroundStyle(Palette.sage)
        case .failed(let reason): Text(reason).foregroundStyle(Palette.rose).lineLimit(1)
        }
    }

    private func test() async {
        guard let candidate else { return }
        check = .checking
        do {
            try await candidate.health()
            check = .ok
        } catch {
            check = .failed(error.friendlyDescription)
        }
    }
}

enum ServerSetting {
    static let key = "serverURL"
    static let defaultURL = AlondraClient.defaultURL.absoluteString
}

private struct PairingSection: View {
    @Binding var detent: PresentationDetent
    @Environment(DevicePairing.self) private var pairing
    @State private var confirmUnpair = false

    var body: some View {
        Section {
            if let url = pairing.pairedURL, let device = pairing.deviceID {
                LabeledContent("Mac") {
                    Text(url.host() ?? url.absoluteString)
                        .font(.mono(.subheadline))
                        .foregroundStyle(Palette.ink)
                }
                LabeledContent("Dispositivo") {
                    Text(device)
                        .font(.mono(.caption))
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                }
                NavigationLink {
                    PairingView()
                        .onAppear { detent = .large }
                } label: {
                    Label("Vincular de nuevo", systemImage: "qrcode.viewfinder")
                }
                Button("Desvincular", systemImage: "link.badge.minus", role: .destructive) { confirmUnpair = true }
                    .confirmationDialog("¿Desvincular este iPhone?", isPresented: $confirmUnpair, titleVisibility: .visible) {
                        Button("Desvincular", role: .destructive) { pairing.unpair() }
                    } message: {
                        Text("Se borra la clave de este iPhone. Para volver a conectarte fuera de este Mac tendrás que vincularlo otra vez.")
                    }
            } else {
                NavigationLink {
                    PairingView()
                        .onAppear { detent = .large }
                } label: {
                    Label {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Vincular con el Mac")
                            Text("Escanea el código de alondra pair")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    } icon: {
                        Image(systemName: "qrcode.viewfinder")
                            .foregroundStyle(Palette.dawn)
                    }
                }
            }
        } header: {
            Text("Este iPhone")
        } footer: {
            if pairing.isPaired {
                Text("Las peticiones fuera de 127.0.0.1 van firmadas con una clave que no sale de este iPhone.")
            } else {
                Text("Sin vincular solo puedes hablar con alondrad en 127.0.0.1.")
            }
        }
    }
}

private struct NotificationsSection: View {
    @Environment(PushCenter.self) private var push
    @Environment(DevicePairing.self) private var pairing
    @Environment(\.openURL) private var openURL

    var body: some View {
        @Bindable var push = push
        Section {
            LabeledContent("Avisos") {
                switch push.authorization {
                case .authorized:
                    Text("\(Image(systemName: "bell.badge.fill")) Activados").foregroundStyle(Palette.sage)
                case .denied:
                    Button("Desactivados · Abrir Ajustes") {
                        if let url = URL(string: UIApplication.openNotificationSettingsURLString) { openURL(url) }
                    }
                    .foregroundStyle(Palette.rose)
                case .notDetermined:
                    Button("Activar") { Task { await push.activate() } }
                case .unknown:
                    ProgressView()
                }
            }
            if push.authorization == .authorized, pairing.isPaired {
                LabeledContent("En el Mac") { registrationStatus }
            }
            Toggle("Horas de silencio", isOn: $push.quietHours.enabled.animation())
            if push.quietHours.enabled {
                DatePicker("Desde", selection: time($push.quietHours.start), displayedComponents: .hourAndMinute)
                DatePicker("Hasta", selection: time($push.quietHours.end), displayedComponents: .hourAndMinute)
            }
        } header: {
            Text("Notificaciones")
        } footer: {
            Text(push.quietHours.enabled
                ? "Entre las \(QuietHours.clock(push.quietHours.start)) y las \(QuietHours.clock(push.quietHours.end)) los avisos llegan sin sonido y sin encender la pantalla."
                : "Puedes responder, permitir o aprobar desde la pantalla bloqueada.")
        }
        .task { await push.refreshAuthorization() }
    }

    @ViewBuilder
    private var registrationStatus: some View {
        switch push.registration {
        case .idle: Text("Esperando token").foregroundStyle(.secondary)
        case .sending: ProgressView()
        case .registered: Text("\(Image(systemName: "checkmark.circle.fill")) Registrado").foregroundStyle(Palette.sage)
        case .failed(let reason): Text(reason).foregroundStyle(Palette.rose).lineLimit(2)
        }
    }

    private func time(_ minutes: Binding<Int>) -> Binding<Date> {
        Binding {
            Calendar.current.date(bySettingHour: minutes.wrappedValue / 60, minute: minutes.wrappedValue % 60, second: 0, of: .now) ?? .now
        } set: { date in
            let parts = Calendar.current.dateComponents([.hour, .minute], from: date)
            minutes.wrappedValue = (parts.hour ?? 0) * 60 + (parts.minute ?? 0)
        }
    }
}
