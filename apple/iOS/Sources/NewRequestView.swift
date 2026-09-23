import AlondraShared
import SwiftUI

struct NewRequestView: View {
    @Environment(Fleet.self) private var fleet
    @State private var text = ""
    @State private var sending = false
    @State private var sentCount = 0
    @FocusState private var focused: Bool

    private var trimmed: String { text.trimmingCharacters(in: .whitespacesAndNewlines) }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("Nuevo")
                            .font(.display(.largeTitle, weight: .bold))
                        orchestratorStatus
                    }
                    editor
                    if !fleet.sentToOrchestrator.isEmpty {
                        history
                    }
                }
                .padding(20)
            }
            .scrollDismissesKeyboard(.interactively)
            .background(DawnBackground(glow: Palette.ember))
            .navigationBarTitleDisplayMode(.inline)
            .safeAreaBar(edge: .bottom) { sendBar }
            .sensoryFeedback(.success, trigger: sentCount)
        }
    }

    private var orchestratorStatus: some View {
        Group {
            if let orchestrator = fleet.orchestrator {
                if orchestrator.alive {
                    Label("Lo recibe \(orchestrator.displayName), que prepara la misión y lanza el nodo.", systemImage: "point.3.connected.trianglepath.dotted")
                        .foregroundStyle(.secondary)
                } else {
                    Label("El orquestador está dormido: lo leerá en cuanto despierte.", systemImage: "moon.zzz")
                        .foregroundStyle(Palette.dawn)
                }
            } else if fleet.overview != nil {
                Label("Ningún orquestador está conectado a Alondra, así que ahora no se puede enviar.", systemImage: "exclamationmark.triangle")
                    .foregroundStyle(Palette.rose)
            }
        }
        .font(.subheadline)
    }

    private var editor: some View {
        ZStack(alignment: .topLeading) {
            if text.isEmpty {
                Text("¿Qué quieres que se haga? Cuéntalo como se lo contarías a alguien del equipo.")
                    .font(.display(.title3, weight: .regular))
                    .foregroundStyle(.tertiary)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 8)
                    .allowsHitTesting(false)
            }
            TextEditor(text: $text)
                .font(.display(.title3, weight: .regular))
                .scrollContentBackground(.hidden)
                .focused($focused)
                .frame(minHeight: 240)
        }
        .padding(16)
        .background(Palette.card, in: .rect(cornerRadius: 28, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 28, style: .continuous).strokeBorder(focused ? Palette.ember.opacity(0.5) : Palette.hairline))
        .shadow(color: .black.opacity(0.12), radius: 18, y: 10)
        .animation(.smooth(duration: 0.25), value: focused)
    }

    private var history: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Enviado")
                .font(.footnote.weight(.bold))
                .textCase(.uppercase)
                .kerning(0.6)
                .foregroundStyle(.secondary)
            ForEach(fleet.sentToOrchestrator.prefix(8)) { message in
                VStack(alignment: .leading, spacing: 8) {
                    Text(requestText(message.text))
                        .font(.subheadline)
                        .lineLimit(4)
                    HStack(spacing: 6) {
                        Image(systemName: message.deliveredAt != nil ? "checkmark.circle.fill" : message.handedAt != nil ? "arrow.right.circle" : "clock")
                        Text(message.deliveredAt != nil ? "Entregado" : message.handedAt != nil ? "En camino" : "En cola")
                        Text("·")
                        Text(relative(message.createdAt))
                    }
                    .font(.caption.weight(.medium))
                    .foregroundStyle(message.deliveredAt != nil ? Palette.sage : .secondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(14)
                .background(Palette.card.opacity(0.8), in: .rect(cornerRadius: 18, style: .continuous))
                .transition(.move(edge: .top).combined(with: .opacity))
            }
        }
    }

    private func requestText(_ raw: String) -> String {
        let prefix = "[Alondra] Petición nueva de David: "
        guard raw.hasPrefix(prefix) else { return raw }
        return String(raw.dropFirst(prefix.count).prefix { $0 != "\n" })
    }

    private var sendBar: some View {
        Button {
            send()
        } label: {
            Label(sending ? "Enviando…" : "Pedírselo al orquestador", systemImage: "paperplane.fill")
                .font(.headline)
                .frame(maxWidth: .infinity)
        }
        .buttonStyle(.glassProminent)
        .buttonBorderShape(.capsule)
        .controlSize(.large)
        .tint(Palette.ember)
        .disabled(trimmed.isEmpty || sending || fleet.orchestrator == nil)
        .padding(.horizontal, 20)
        .padding(.vertical, 8)
    }

    private func send() {
        let request = trimmed
        sending = true
        Task {
            let ok = await fleet.perform { try await $0.newRequest(request) }
            sending = false
            if ok {
                withAnimation(.spring(duration: 0.4)) { text = "" }
                focused = false
                sentCount += 1
                fleet.notice = Notice("Enviado al orquestador")
            }
        }
    }
}
