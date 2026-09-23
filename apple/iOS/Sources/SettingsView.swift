import SwiftUI

struct SettingsView: View {
    @Environment(\.dismiss) private var dismiss
    @AppStorage(ServerSetting.key) private var serverURL = FleetClient.defaultURL
    @State private var draft = ""
    @State private var check: Check = .idle

    private enum Check: Equatable {
        case idle, checking, ok, failed(String)
    }

    private var candidate: FleetClient? { FleetClient(string: draft) }

    var body: some View {
        NavigationStack {
            Form {
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
                    Button("Volver a la dirección local") { draft = FleetClient.defaultURL }
                        .disabled(draft == FleetClient.defaultURL)
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
            .presentationDetents([.medium, .large])
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
}
