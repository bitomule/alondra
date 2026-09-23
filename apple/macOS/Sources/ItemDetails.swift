import AlondraShared
import SwiftUI

struct ItemDetail: View {
    let item: InboxItem
    let store: FleetStore
    let drafts: Drafts

    var body: some View {
        switch item {
        case .question(let q):
            QuestionDetail(question: q, store: store, drafts: drafts, draft: drafts.draft(for: item.id))
        case .report(let r):
            ReportDetail(report: r, store: store, drafts: drafts, draft: drafts.draft(for: item.id))
        }
    }
}

struct QuestionDetail: View {
    let question: Question
    let store: FleetStore
    let drafts: Drafts
    @Bindable var draft: ReviewDraft
    @FocusState private var focus: DetailField?
    @State private var pendingOption: String?
    @State private var armed = false
    @Environment(\.searchActive) private var searchActive

    private var key: String { "q\(question.id)" }
    private var textAttachments: [Attachment] { question.attachments.filter(\.isText) }
    private var otherAttachments: [Attachment] { question.attachments.filter { !$0.isText } }
    private var feedback: String { draft.feedback }

    var body: some View {
        DetailScaffold {
            VStack(alignment: .leading, spacing: 22) {
                DetailHeader(style: InboxItem.question(question).style, node: question.nodeName, date: question.askedAt, title: question.title)
                if let detail = question.detail, !detail.isEmpty {
                    if question.isPlan {
                        ReviewableDocument(text: detail, anchorPrefix: "plan", draft: draft, focus: $focus)
                    } else if question.origin == "permission" {
                        MarkdownText(text: "```\n\(detail)\n```")
                    } else {
                        MarkdownText(text: detail)
                    }
                }
                ForEach(Array(textAttachments.enumerated()), id: \.element) { index, attachment in
                    AttachmentDocument(attachment: attachment, anchorPrefix: "adjunto\(index)", store: store, draft: draft, focus: $focus)
                }
                LinksRow(links: question.links, attachments: otherAttachments)
                if let context = question.nodeContext?.trimmingCharacters(in: .whitespacesAndNewlines), !context.isEmpty {
                    DisclosureGroup("Lo último que dijo el nodo") {
                        MarkdownText(text: context)
                            .font(.callout)
                            .foregroundStyle(.secondary)
                            .padding(.top, 6)
                    }
                    .foregroundStyle(.secondary)
                }
            }
        } bar: {
            VStack(alignment: .leading, spacing: 12) {
                CommentsSummary(draft: draft)
                if !question.options.isEmpty {
                    options
                }
                Composer(
                    placeholder: placeholder,
                    text: $draft.general,
                    focus: $focus,
                    sendTitle: sendTitle,
                    canSend: !feedback.isEmpty,
                    send: submit,
                    escape: { pendingOption = nil }
                )
                HStack(spacing: 12) {
                    Button {
                        let note = feedback
                        resolve { await store.moreContext(question, note: note) }
                    } label: {
                        Label("Necesito más contexto", systemImage: "questionmark.bubble")
                    }
                    .buttonStyle(.link)
                    .help("Devuelve la pregunta al nodo para que la explique mejor. Lo que hayas escrito va como lo que te falta.")
                    Spacer()
                    Hint(text: hint)
                }
                .font(.callout)
            }
            .animation(.snappy(duration: 0.2), value: draft.comments.count)
        }
        .task {
            if !searchActive { focus = question.options.isEmpty && !question.isPlan ? .reply : .pane }
            try? await Task.sleep(for: .milliseconds(450))
            armed = true
        }
        .triageKeys(focus: $focus, handle: handleKey)
    }

    private var options: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 10) { optionButtons }
            VStack(spacing: 8) { optionButtons }
        }
    }

    private var optionButtons: some View {
        ForEach(Array(question.options.enumerated()), id: \.offset) { index, option in
            OptionButton(
                index: index,
                label: option.label,
                detail: optionDetail(option),
                prominent: isProminent(option, index: index),
                selected: pendingOption == option.label,
                action: { choose(option) }
            )
        }
    }

    private func needsText(_ option: QuestionOption) -> Bool {
        option.description?.lowercased().hasPrefix("escribe") == true
    }

    private func optionDetail(_ option: QuestionOption) -> String? {
        let comments = draft.comments.count
        guard comments > 0 else { return option.description }
        if needsText(option) || !question.isPlan {
            return comments == 1 ? "Envía tu comentario" : "Envía tus \(comments) comentarios"
        }
        return "Sin tus comentarios"
    }

    private func isProminent(_ option: QuestionOption, index: Int) -> Bool {
        guard question.isPlan else { return false }
        return draft.hasFeedback ? needsText(option) : index == 0
    }

    private var placeholder: String {
        if let pendingOption { return "\(pendingOption): escribe por qué…" }
        if question.isPlan { return "Comentario general sobre el plan (opcional)…" }
        return question.options.isEmpty ? "Tu respuesta…" : "O responde con tus palabras…"
    }

    private var sendTitle: String? {
        if let pendingOption { return pendingOption }
        return question.isPlan ? nil : "Responder"
    }

    private var hint: String {
        if focus == .reply { return "↩ envía · ⌥↩ nueva línea · esc sale" }
        if question.isPlan { return "Pasa el ratón por un bloque para comentarlo · 1–2 decide" }
        let count = question.options.count
        switch count {
        case 0: return "R escribe · ↩ envía"
        case 1: return "1 elige · R escribe"
        default: return "1–\(min(count, 9)) elige · R escribe"
        }
    }

    private func handleKey(_ character: Character) -> Bool {
        guard armed, draft.editing == nil else { return false }
        if character == "r" {
            focus = .reply
            return true
        }
        guard let number = character.wholeNumberValue, number >= 1, number <= question.options.count else { return false }
        choose(question.options[number - 1])
        return true
    }

    private func choose(_ option: QuestionOption) {
        let text = feedback
        if needsText(option) && text.isEmpty {
            pendingOption = option.label
            focus = .reply
            return
        }
        answer(text.isEmpty ? option.label : text, option: option.label)
    }

    private func submit() {
        let text = feedback
        guard !text.isEmpty else { return }
        if let pendingOption {
            answer(text, option: pendingOption)
        } else if question.isPlan, let change = question.options.first(where: needsText) ?? question.options.dropFirst().first {
            answer(text, option: change.label)
        } else {
            answer(text, option: nil)
        }
    }

    private func answer(_ text: String, option: String?) {
        resolve { await store.answer(question, text: text, option: option) }
    }

    private func resolve(_ action: @escaping @MainActor () async -> Bool) {
        focus = .pane
        let drafts = drafts
        let key = key
        Task {
            if await action() { drafts.discard(key) }
        }
    }
}

struct ReportDetail: View {
    let report: Report
    let store: FleetStore
    let drafts: Drafts
    @Bindable var draft: ReviewDraft
    @FocusState private var focus: DetailField?
    @State private var armed = false
    @Environment(\.searchActive) private var searchActive

    private var key: String { "r\(report.id)" }
    private var isDocument: Bool { report.kind == "document" }
    private var textAttachments: [Attachment] { isDocument ? report.attachments.filter(\.isText) : [] }
    private var otherAttachments: [Attachment] { report.attachments.filter { !textAttachments.contains($0) } }
    private var feedback: String { draft.feedback }

    var body: some View {
        DetailScaffold {
            VStack(alignment: .leading, spacing: 22) {
                DetailHeader(style: InboxItem.report(report).style, node: report.nodeName, date: report.createdAt, title: report.title)
                if let body = report.body, !body.isEmpty {
                    if isDocument {
                        ReviewableDocument(text: body, anchorPrefix: "cuerpo", draft: draft, focus: $focus)
                    } else {
                        MarkdownText(text: body)
                    }
                }
                ForEach(Array(textAttachments.enumerated()), id: \.element) { index, attachment in
                    AttachmentDocument(attachment: attachment, anchorPrefix: "adjunto\(index)", store: store, draft: draft, focus: $focus)
                }
                LinksRow(links: report.links, attachments: otherAttachments)
            }
        } bar: {
            VStack(alignment: .leading, spacing: 12) {
                CommentsSummary(draft: draft)
                HStack(alignment: .bottom, spacing: 8) {
                    Composer(
                        placeholder: isDocument ? "Comentario general (opcional)…" : "Responder al nodo (opcional)…",
                        text: $draft.general,
                        focus: $focus,
                        sendTitle: isDocument ? "Enviar comentarios" : "Responder",
                        canSend: !feedback.isEmpty,
                        send: sendFeedback
                    )
                    ReadButton(prominent: feedback.isEmpty, action: markRead)
                }
                HStack {
                    Spacer()
                    Hint(text: hint)
                }
            }
            .animation(.snappy(duration: 0.2), value: draft.comments.count)
        }
        .task {
            if !searchActive { focus = .pane }
            try? await Task.sleep(for: .milliseconds(450))
            armed = true
        }
        .triageKeys(focus: $focus, handle: handleKey)
    }

    private func handleKey(_ character: Character) -> Bool {
        guard armed, draft.editing == nil else { return false }
        switch character {
        case "r": focus = .reply
        case "l": markRead()
        default: return false
        }
        return true
    }

    private var hint: String {
        if focus == .reply { return "↩ envía · ⌥↩ nueva línea · esc sale" }
        if isDocument { return "Pasa el ratón por un bloque para comentarlo · L leído" }
        return "L leído · R escribe"
    }

    private func sendFeedback() {
        let text = feedback
        guard !text.isEmpty else { return }
        resolve { await store.reply(to: report, text: text) }
    }

    private func markRead() {
        resolve { await store.markRead(report) }
    }

    private func resolve(_ action: @escaping @MainActor () async -> Bool) {
        focus = .pane
        let drafts = drafts
        let key = key
        Task {
            if await action() { drafts.discard(key) }
        }
    }
}

struct ReadButton: View {
    let prominent: Bool
    let action: () -> Void

    var body: some View {
        Group {
            if prominent {
                button.buttonStyle(.borderedProminent)
            } else {
                button.buttonStyle(.bordered)
            }
        }
        .controlSize(.large)
        .help("Marcar como leído (L)")
    }

    private var button: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                Text("Leído")
                KeyCap(key: "L", onAccent: prominent)
            }
            .padding(.horizontal, 2)
        }
    }
}
