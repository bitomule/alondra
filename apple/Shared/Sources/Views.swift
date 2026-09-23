import SwiftUI

enum Selection: Hashable {
    case item(String)
    case node(String)
}

public struct FleetView: View {
    @State private var store = FleetStore()
    @State private var selection: Selection?
    @State private var query = ""
    @State private var smartMatches: Set<String>?
    @State private var smartVia = ""
    @State private var showingNew = false

    public init() {}

    public var body: some View {
        NavigationSplitView {
            List(selection: $selection) {
                Section("Te espera (\(store.inbox.count))") {
                    if store.inbox.isEmpty {
                        Label("Nada pendiente", systemImage: "checkmark.seal").foregroundStyle(.secondary)
                    }
                    ForEach(store.inbox) { item in
                        InboxRow(item: item).tag(Selection.item(item.id))
                    }
                }
                ForEach(filteredProjects, id: \.name) { project in
                    Section(project.name) {
                        ForEach(project.nodes) { node in
                            NodeRow(node: node, pending: store.items(for: node.sessionId).count)
                                .tag(Selection.node(node.sessionId))
                        }
                    }
                }
            }
            .searchable(text: $query, placement: .sidebar, prompt: "Buscar nodos…")
            .task(id: query) { await runSmartSearch() }
            .safeAreaInset(edge: .bottom) { MachineBar(store: store, smartVia: smartVia) }
            .navigationSplitViewColumnWidth(min: 300, ideal: 360)
        } detail: {
            switch selection {
            case .item(let id):
                if let item = store.inbox.first(where: { $0.id == id }) {
                    ItemDetail(item: item, store: store, done: { advance(from: id) })
                        .id(item.id)
                } else {
                    Resolved(next: { selection = store.inbox.first.map { .item($0.id) } }, remaining: store.inbox.count)
                }
            case .node(let id):
                if let node = store.nodes.first(where: { $0.sessionId == id }) {
                    NodeDetail(node: node, store: store, open: { selection = .item($0) })
                        .id(node.sessionId)
                } else {
                    ContentUnavailableView("Sin nodo", systemImage: "questionmark.circle")
                }
            case nil:
                Resolved(next: { selection = store.inbox.first.map { .item($0.id) } }, remaining: store.inbox.count)
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    showingNew = true
                } label: {
                    Label("Nuevo", systemImage: "plus")
                }
                .keyboardShortcut("n")
                .help("Pedir trabajo nuevo al orquestador (⌘N)")
            }
        }
        .sheet(isPresented: $showingNew) {
            NewRequestSheet(store: store, isPresented: $showingNew)
        }
        .task { store.start() }
        .alert("Alondra", isPresented: Binding(get: { store.lastError != nil }, set: { if !$0 { store.lastError = nil } })) {
            Button("Vale", role: .cancel) {}
        } message: {
            Text(store.lastError ?? "")
        }
    }

    private func advance(from id: String) {
        let items = store.inbox
        if let index = items.firstIndex(where: { $0.id == id }), index + 1 < items.count {
            selection = .item(items[index + 1].id)
        } else if let first = items.first(where: { $0.id != id }) {
            selection = .item(first.id)
        } else {
            selection = nil
        }
    }

    private var filteredProjects: [(name: String, nodes: [Node])] {
        let q = query.trimmingCharacters(in: .whitespaces).lowercased()
        guard !q.isEmpty else { return store.projects }
        return store.projects.compactMap { project in
            let nodes = project.nodes.filter { node in
                if let smartMatches { return smartMatches.contains(node.sessionId) }
                let haystack = "\(node.displayName) \(project.name) \(node.state.label) \(node.summary)".lowercased()
                return q.split(separator: " ").allSatisfy { haystack.contains($0) }
            }
            return nodes.isEmpty ? nil : (name: project.name, nodes: nodes)
        }
    }

    private func runSmartSearch() async {
        smartMatches = nil
        smartVia = ""
        let q = query.trimmingCharacters(in: .whitespaces)
        guard q.count >= 3 else { return }
        try? await Task.sleep(for: .milliseconds(1200))
        guard !Task.isCancelled else { return }
        let literalHits = filteredProjects.reduce(0) { $0 + $1.nodes.count }
        guard literalHits == 0 else { return }
        smartVia = "Buscando con jev…"
        if let result = await store.smartSearch(q), !Task.isCancelled {
            smartMatches = Set(result.sessions)
            smartVia = result.via == "jev" ? "Resultados de jev" : ""
        }
    }
}

struct Resolved: View {
    let next: () -> Void
    let remaining: Int

    var body: some View {
        if remaining == 0 {
            ContentUnavailableView("Nada pendiente", systemImage: "checkmark.seal", description: Text("Ningún nodo te necesita ahora mismo."))
        } else {
            ContentUnavailableView {
                Label("\(remaining) pendientes", systemImage: "tray.full")
            } actions: {
                Button("Empezar", action: next).keyboardShortcut(.defaultAction).controlSize(.large)
            }
        }
    }
}

struct NodeRow: View {
    let node: Node
    let pending: Int

    var body: some View {
        HStack(spacing: 6) {
            Circle().fill(node.state.color).frame(width: 8, height: 8)
            if node.isOrchestrator { Image(systemName: "star.fill").foregroundStyle(.yellow).font(.caption) }
            Text(shortName).lineLimit(1)
            if !node.alive { Text("💤").font(.caption) }
            Spacer()
            if pending > 0 {
                Text("\(pending)").font(.caption.bold()).padding(.horizontal, 6).background(.yellow.opacity(0.35), in: .capsule)
            }
        }
        .help("\(node.displayName) · \(node.state.label)")
    }

    private var shortName: String {
        let name = node.displayName
        if let last = name.split(separator: "/").last, name.contains("/") { return String(last) }
        return name
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
    let smartVia: String

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            if !smartVia.isEmpty { Text(smartVia).foregroundStyle(.purple) }
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
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.bar)
    }
}

struct InboxRow: View {
    let item: InboxItem

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text(badge).font(.caption2.bold()).padding(.horizontal, 6).padding(.vertical, 1).background(tint.opacity(0.25), in: .capsule)
                Text(node).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                Spacer()
                Text(ago(item.date)).font(.caption2).foregroundStyle(.tertiary)
            }
            Text(title).lineLimit(2)
        }
        .padding(.vertical, 2)
    }

    private var badge: String {
        switch item {
        case .question(let q): q.kindLabel ?? "Pregunta"
        case .report(let r): r.kindLabel
        }
    }

    private var tint: Color {
        switch item {
        case .question: .yellow
        case .report(let r): r.kind == "document" ? .blue : .cyan
        }
    }

    private var title: String {
        switch item {
        case .question(let q): q.title
        case .report(let r): r.title
        }
    }

    private var node: String {
        let name: String
        switch item {
        case .question(let q): name = q.nodeName ?? ""
        case .report(let r): name = r.nodeName ?? ""
        }
        return String(name.split(separator: "/").last ?? Substring(name))
    }
}

struct ItemDetail: View {
    let item: InboxItem
    let store: FleetStore
    let done: () -> Void

    var body: some View {
        switch item {
        case .question(let q): QuestionDetail(question: q, store: store, done: done)
        case .report(let r): ReportDetail(report: r, store: store, done: done)
        }
    }
}

struct Markdown: View {
    let text: String

    var body: some View {
        Text((try? AttributedString(markdown: text, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(text))
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct Links: View {
    let links: [Link]
    let attachments: [Attachment]

    var body: some View {
        if !links.isEmpty || !attachments.isEmpty {
            HStack(spacing: 14) {
                ForEach(links, id: \.self) { link in
                    if let url = URL(string: link.url) {
                        SwiftUI.Link(destination: url) { Label(link.title ?? link.url, systemImage: "arrow.up.right.square") }
                    }
                }
                ForEach(attachments, id: \.self) { file in
                    Button {
                        NSWorkspace.shared.open(URL(fileURLWithPath: file.storedPath ?? file.sourcePath))
                    } label: {
                        Label(file.name, systemImage: "doc.text")
                    }
                    .buttonStyle(.link)
                }
            }
        }
    }
}

struct Header: View {
    let kind: String
    let node: String
    let date: Int64
    let title: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("\(kind) · \(node) · hace \(ago(date))").font(.subheadline).foregroundStyle(.secondary)
            Text(title).font(.title2.bold()).textSelection(.enabled)
        }
    }
}

struct QuestionDetail: View {
    let question: Question
    let store: FleetStore
    let done: () -> Void
    @State private var reply = ""
    @FocusState private var replyFocused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Header(kind: question.kindLabel ?? "Pregunta", node: question.nodeName ?? question.sessionId, date: question.askedAt, title: question.title)
                    if let detail = question.detail, !detail.isEmpty { Markdown(text: detail) }
                    Links(links: question.links, attachments: question.attachments)
                    if let context = question.nodeContext, !context.isEmpty {
                        DisclosureGroup("Lo que hacía el nodo") {
                            Text(context).font(.callout).foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled)
                        }
                    }
                }
                .padding(28)
                .frame(maxWidth: 780, alignment: .leading)
            }
            Divider()
            VStack(alignment: .leading, spacing: 12) {
                if !question.options.isEmpty {
                    HStack(spacing: 10) {
                        ForEach(Array(question.options.enumerated()), id: \.offset) { index, option in
                            Button {
                                answer(option.label, option: option.label)
                            } label: {
                                VStack(spacing: 2) {
                                    Text(option.label).font(.headline)
                                    if let d = option.description { Text(d).font(.caption).foregroundStyle(.secondary).lineLimit(2) }
                                }
                                .frame(maxWidth: .infinity)
                                .padding(.vertical, 6)
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.large)
                            .keyboardShortcut(KeyEquivalent(Character("\(index + 1)")), modifiers: [])
                            .help("Tecla \(index + 1)")
                        }
                    }
                }
                HStack(alignment: .bottom) {
                    TextField(question.options.isEmpty ? "Tu respuesta…" : "O responde con tus palabras…", text: $reply, axis: .vertical)
                        .lineLimit(1...6)
                        .textFieldStyle(.roundedBorder)
                        .focused($replyFocused)
                        .onSubmit(sendReply)
                    Button("Responder", action: sendReply)
                        .keyboardShortcut(.return, modifiers: .command)
                        .disabled(reply.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
                HStack {
                    Button {
                        let note = reply.trimmingCharacters(in: .whitespacesAndNewlines)
                        reply = ""
                        Task { await store.moreContext(question, note: note) }
                        done()
                    } label: {
                        Label("Necesito más contexto", systemImage: "questionmark.bubble")
                    }
                    .help("Devuelve la pregunta al nodo para que la explique mejor. Si escribes algo arriba, se lo mando como lo que te falta.")
                    Spacer()
                    Text("1–\(max(question.options.count, 1)) elige · ⌘↩ responde").font(.caption).foregroundStyle(.tertiary)
                }
            }
            .padding(20)
            .background(.bar)
        }
        .onAppear { if question.options.isEmpty { replyFocused = true } }
    }

    private func answer(_ text: String, option: String?) {
        Task { await store.answer(question, text: text, option: option) }
        done()
    }

    private func sendReply() {
        let text = reply.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        reply = ""
        answer(text, option: nil)
    }
}

struct ReportDetail: View {
    let report: Report
    let store: FleetStore
    let done: () -> Void
    @State private var reply = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Header(kind: report.kindLabel, node: report.nodeName ?? "", date: report.createdAt, title: report.title)
                    if let body = report.body, !body.isEmpty { Markdown(text: body) }
                    Links(links: report.links, attachments: report.attachments)
                }
                .padding(28)
                .frame(maxWidth: 780, alignment: .leading)
            }
            Divider()
            HStack(alignment: .bottom) {
                TextField(report.kind == "document" ? "Comentarios para el nodo (opcional)…" : "Responder al nodo (opcional)…", text: $reply, axis: .vertical)
                    .lineLimit(1...6)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit(sendReply)
                if !reply.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    Button("Enviar", action: sendReply).keyboardShortcut(.return, modifiers: .command)
                }
                Button("Leído") {
                    Task { await store.markRead(report) }
                    done()
                }
                .keyboardShortcut("l", modifiers: [])
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .help("Marcar como leído (L)")
            }
            .padding(20)
            .background(.bar)
        }
    }

    private func sendReply() {
        let text = reply.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        reply = ""
        Task { await store.reply(to: report, text: text) }
        done()
    }
}

struct NodeDetail: View {
    let node: Node
    let store: FleetStore
    let open: (String) -> Void
    @State private var message = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(node.project ?? "").font(.subheadline).foregroundStyle(.secondary)
                        Text(node.displayName).font(.title2.bold())
                        HStack {
                            Circle().fill(node.state.color).frame(width: 9, height: 9)
                            Text("\(node.state.label) · hace \(ago(node.stateSince))")
                            if !node.alive { Text("· 💤 dormido").foregroundStyle(.secondary) }
                        }
                    }
                    let items = store.items(for: node.sessionId)
                    if !items.isEmpty {
                        GroupBox("Te espera") {
                            VStack(alignment: .leading) {
                                ForEach(items) { item in
                                    Button { open(item.id) } label: { InboxRow(item: item) }.buttonStyle(.plain)
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                    if let tool = node.tool {
                        GroupBox("Ahora") {
                            Text("\(tool.name) \(tool.summary ?? "") · hace \(ago(tool.since))").frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled)
                        }
                    }
                    if let last = node.lastMessage ?? node.daemonDetail {
                        GroupBox("Lo último que dijo") { Markdown(text: last).font(.callout) }
                    }
                    Text(node.cwd ?? "").font(.caption).foregroundStyle(.tertiary).textSelection(.enabled)
                }
                .padding(28)
                .frame(maxWidth: 780, alignment: .leading)
            }
            Divider()
            HStack(alignment: .bottom) {
                TextField(node.alive ? "Escribe a \(node.displayName)…" : "Escribe (lo despierta)…", text: $message, axis: .vertical)
                    .lineLimit(1...6)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit(send)
                Button("Enviar", action: send).disabled(message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            .padding(20)
            .background(.bar)
        }
    }

    private func send() {
        let text = message.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        message = ""
        Task { await store.send(to: node, text: text) }
    }
}

struct NewRequestSheet: View {
    let store: FleetStore
    @Binding var isPresented: Bool
    @State private var text = ""
    @State private var sending = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("¿Qué quieres que se haga?").font(.title3.bold())
            Text("Se lo paso al orquestador: preparará la misión y lanzará el nodo.").foregroundStyle(.secondary)
            TextEditor(text: $text)
                .font(.body)
                .frame(minWidth: 480, minHeight: 160)
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(.quaternary))
            HStack {
                Spacer()
                Button("Cancelar", role: .cancel) { isPresented = false }
                Button("Enviar") {
                    sending = true
                    Task {
                        if await store.newRequest(text) { isPresented = false }
                        sending = false
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(sending || text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(20)
    }
}
