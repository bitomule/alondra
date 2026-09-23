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
    private var feedback: String { draft.feedback(author: store.userName) }

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
                    DisclosureGroup("What the node said last") {
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
                        Label("I need more context", systemImage: "questionmark.bubble")
                    }
                    .buttonStyle(.link)
                    .help("Sends the question back to the node so it explains it better. Whatever you wrote goes along as what you’re missing.")
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
                label: option.displayLabel,
                detail: optionDetail(option),
                prominent: isProminent(option, index: index),
                selected: pendingOption == option.label,
                action: { choose(option) }
            )
        }
    }

    private func needsText(_ option: QuestionOption) -> Bool {
        ["write", "escribe"].contains { option.description?.lowercased().hasPrefix($0) == true }
    }

    private func optionDetail(_ option: QuestionOption) -> String? {
        let comments = draft.comments.count
        guard comments > 0 else { return option.displayDescription }
        if needsText(option) || !question.isPlan {
            return String(localized: "Sends your \(comments) comments")
        }
        return String(localized: "Without your comments")
    }

    private func isProminent(_ option: QuestionOption, index: Int) -> Bool {
        guard question.isPlan else { return false }
        return draft.hasFeedback ? needsText(option) : index == 0
    }

    private var placeholder: String {
        if let pendingOption { return String(localized: "\(AnswerOption.displayLabel(pendingOption)): write why…") }
        if question.isPlan { return String(localized: "General comment on the plan (optional)…") }
        return question.options.isEmpty ? String(localized: "Your answer…") : String(localized: "Or answer in your own words…")
    }

    private var sendTitle: String? {
        if let pendingOption { return AnswerOption.displayLabel(pendingOption) }
        return question.isPlan ? nil : String(localized: "Reply")
    }

    private var hint: String {
        if focus == .reply { return String(localized: "↩ sends · ⌥↩ new line · esc leaves") }
        if question.isPlan { return String(localized: "Hover over a block to comment on it · 1–2 decides") }
        let count = question.options.count
        switch count {
        case 0: return String(localized: "R writes · ↩ sends")
        case 1: return String(localized: "1 picks · R writes")
        default: return String(localized: "1–\(min(count, 9)) picks · R writes")
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
    private var feedback: String { draft.feedback(author: store.userName) }

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
                        placeholder: isDocument ? String(localized: "General comment (optional)…") : String(localized: "Reply to the node (optional)…"),
                        text: $draft.general,
                        focus: $focus,
                        sendTitle: isDocument ? String(localized: "Send Comments") : String(localized: "Reply"),
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
        if focus == .reply { return String(localized: "↩ sends · ⌥↩ new line · esc leaves") }
        if isDocument { return String(localized: "Hover over a block to comment on it · L marks as read") }
        return String(localized: "L marks as read · R writes")
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
        .help("Mark as read (L)")
    }

    private var button: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                Text("Mark as Read")
                KeyCap(key: "L", onAccent: prominent)
            }
            .padding(.horizontal, 2)
        }
    }
}
