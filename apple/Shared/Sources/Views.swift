import SwiftUI

enum Selection: Hashable {
    case question(UInt64)
    case node(String)
}

public struct FleetView: View {
    @State private var store = FleetStore()
    @State private var selection: Selection?

    public init() {}

    public var body: some View {
        NavigationSplitView {
            List(selection: $selection) {
                Section("Te esperan (\(store.questions.count))") {
                    if store.questions.isEmpty {
                        Text("Nada pendiente").foregroundStyle(.secondary)
                    }
                    ForEach(store.questions) { question in
                        QuestionRow(question: question).tag(Selection.question(question.id))
                    }
                }
                Section("Flota (\(store.nodes.count))") {
                    ForEach(store.nodes) { node in
                        NodeRow(node: node).tag(Selection.node(node.sessionId))
                    }
                }
            }
            .navigationSplitViewColumnWidth(min: 300, ideal: 380)
            .safeAreaInset(edge: .bottom) { MachineBar(store: store) }
        } detail: {
            switch selection {
            case .question(let id):
                if let question = store.questions.first(where: { $0.id == id }) {
                    QuestionDetail(question: question, store: store)
                } else {
                    ContentUnavailableView("Contestada", systemImage: "checkmark.circle", description: Text("Esta pregunta ya no está abierta."))
                }
            case .node(let id):
                if let node = store.nodes.first(where: { $0.sessionId == id }) {
                    NodeDetail(node: node, store: store)
                } else {
                    ContentUnavailableView("Sin nodo", systemImage: "questionmark.circle")
                }
            case nil:
                OrchestratorPane(store: store)
            }
        }
        .task { store.start() }
        .alert("Alondra", isPresented: Binding(get: { store.lastError != nil }, set: { if !$0 { store.lastError = nil } })) {
            Button("Vale", role: .cancel) {}
        } message: {
            Text(store.lastError ?? "")
        }
    }
}

struct QuestionRow: View {
    let question: Question

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                Text("#\(question.id)").monospacedDigit().foregroundStyle(.secondary)
                if let kind = question.kindLabel {
                    Text(kind).font(.caption).padding(.horizontal, 5).background(.yellow.opacity(0.25), in: .capsule)
                }
                Text(question.title).lineLimit(2)
            }
            Text("\(question.nodeName ?? "") · hace \(ago(question.askedAt))").font(.caption).foregroundStyle(.secondary)
        }
    }
}

struct NodeRow: View {
    let node: Node

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Circle().fill(node.state.color).frame(width: 8, height: 8)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    if node.isOrchestrator { Image(systemName: "star.fill").foregroundStyle(.yellow) }
                    Text(node.displayName).lineLimit(1)
                    if !node.alive { Text("💤") }
                }
                Text("\(node.state.label) · \(ago(node.stateSince))").font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}

extension NodeState {
    var color: Color {
        switch self {
        case .waitingAnswer: .yellow
        case .unclear: .purple
        case .delivered: .cyan
        case .waitingMachine: .orange
        case .working: .green
        case .done: .gray
        }
    }
}

struct MachineBar: View {
    let store: FleetStore

    var body: some View {
        HStack {
            Circle().fill(store.connected ? .green : .red).frame(width: 7, height: 7)
            if let machine = store.overview?.machine {
                let memory = machine.memoryFreePercent.map { "\($0)% libre" } ?? "\(machine.memoryAvailable / 1_073_741_824) GB libres"
                Text("Memoria \(memory) · carga \(Int(machine.load.first ?? 0))")
                if machine.unreliable { Image(systemName: "exclamationmark.triangle").help("Carga alta: las medidas pueden no ser fiables") }
            } else {
                Text(store.connected ? "" : "Sin conexión con alondrad")
            }
            Spacer()
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.bar)
    }
}

struct QuestionDetail: View {
    let question: Question
    let store: FleetStore
    @State private var reply = ""

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("#\(question.id) · \(question.nodeName ?? question.sessionId)").font(.subheadline).foregroundStyle(.secondary)
                    Text(question.title).font(.title2.bold()).textSelection(.enabled)
                }
                if let detail = question.detail, !detail.isEmpty {
                    Text((try? AttributedString(markdown: detail, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(detail))
                        .textSelection(.enabled)
                }
                if !question.links.isEmpty || !question.attachments.isEmpty {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(question.links, id: \.self) { link in
                            if let url = URL(string: link.url) {
                                SwiftUI.Link(destination: url) { Label(link.title ?? link.url, systemImage: "arrow.up.right.square") }
                            }
                        }
                        ForEach(question.attachments, id: \.self) { file in
                            Button {
                                let path = file.storedPath ?? file.sourcePath
                                NSWorkspace.shared.open(URL(fileURLWithPath: path))
                            } label: {
                                Label(file.name, systemImage: "paperclip")
                            }
                            .buttonStyle(.link)
                        }
                    }
                }
                if !question.options.isEmpty {
                    HStack {
                        ForEach(question.options, id: \.self) { option in
                            Button(option.label) {
                                Task { await store.answer(question, text: option.label, option: option.label) }
                            }
                            .help(option.description ?? "")
                            .controlSize(.large)
                        }
                    }
                }
                HStack {
                    TextField("Responder con texto…", text: $reply, axis: .vertical)
                        .textFieldStyle(.roundedBorder)
                        .onSubmit(send)
                    Button("Enviar", action: send).disabled(reply.trimmingCharacters(in: .whitespaces).isEmpty)
                }
                if let context = question.nodeContext, !context.isEmpty {
                    GroupBox("Lo que hacía el nodo") {
                        Text(context).font(.callout).foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled)
                    }
                }
            }
            .padding(24)
            .frame(maxWidth: 760, alignment: .leading)
        }
    }

    private func send() {
        let text = reply.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        reply = ""
        Task { await store.answer(question, text: text) }
    }
}

struct NodeDetail: View {
    let node: Node
    let store: FleetStore
    @State private var message = ""

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Text(node.displayName).font(.title2.bold())
                        if !node.alive { Text("💤 dormido").foregroundStyle(.secondary) }
                    }
                    HStack {
                        Circle().fill(node.state.color).frame(width: 9, height: 9)
                        Text("\(node.state.label) desde hace \(ago(node.stateSince))")
                        Text(sourceLabel).foregroundStyle(.secondary)
                    }
                    Text(node.cwd ?? "").font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                }
                if let tool = node.tool {
                    GroupBox("Ahora") {
                        Text("\(tool.name) \(tool.summary ?? "") · hace \(ago(tool.since))").frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled)
                    }
                }
                if let needs = node.daemonNeeds, !needs.isEmpty {
                    GroupBox("Necesita") { Text(needs).frame(maxWidth: .infinity, alignment: .leading) }
                }
                if let last = node.lastMessage ?? node.daemonDetail {
                    GroupBox("Último mensaje") {
                        Text((try? AttributedString(markdown: last, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(last))
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .textSelection(.enabled)
                    }
                }
                HStack {
                    TextField(node.alive ? "Mensaje al nodo…" : "Mensaje (lo despierta con attach)…", text: $message, axis: .vertical)
                        .textFieldStyle(.roundedBorder)
                        .onSubmit(send)
                    Button("Enviar", action: send).disabled(message.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
            .padding(24)
            .frame(maxWidth: 760, alignment: .leading)
        }
    }

    private var sourceLabel: String {
        switch node.stateSource {
        case "daemon": "(según el demonio)"
        case "classifier": "(según jev)"
        default: ""
        }
    }

    private func send() {
        let text = message.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        message = ""
        Task { await store.send(to: node, text: text) }
    }
}

struct OrchestratorPane: View {
    let store: FleetStore

    var body: some View {
        if let orchestrator = store.orchestrator {
            NodeDetail(node: orchestrator, store: store)
        } else {
            ContentUnavailableView(
                "Elige una pregunta o un nodo",
                systemImage: "bird",
                description: Text("El orquestador aparecerá aquí cuando esté conectado a Alondra.")
            )
        }
    }
}
