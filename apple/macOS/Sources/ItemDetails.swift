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
        case .pullRequest(let p):
            PullRequestDetail(pr: p, store: store, drafts: drafts, draft: drafts.draft(for: item.id))
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
    private var canSend: Bool { !feedback.isEmpty || !draft.attachments.isEmpty }

    var body: some View {
        DetailScaffold {
            VStack(alignment: .leading, spacing: 22) {
                DetailHeader(style: InboxItem.question(question).style, node: question.nodeName, date: question.askedAt, title: question.title)
                MissionLine(item: .question(question))
                if let detail = question.readableDetail, !detail.isEmpty {
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
                ForEach(question.aboutReports) { report in
                    GroupBox {
                        VStack(alignment: .leading, spacing: 8) {
                            Text(report.title)
                                .font(.headline)
                            if let body = report.body, !body.isEmpty {
                                MarkdownText(text: body)
                                    .font(.callout)
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                    } label: {
                        Label(String(localized: "About report #\(report.id)"), systemImage: "doc.text")
                            .foregroundStyle(.secondary)
                    }
                }
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
                    QuestionOptions(options: question.options, selected: pendingOption, detail: optionDetail, prominent: isProminent, choose: choose)
                }
                Composer(
                    placeholder: placeholder,
                    text: $draft.general,
                    focus: $focus,
                    sendTitle: nil,
                    canSend: canSend,
                    send: submit,
                    escape: { pendingOption = nil },
                    attachments: draft.attachments,
                    client: store.client
                )
                CardActions {
                    Button {
                        let note = feedback
                        let attachments = draft.attachments
                        resolve { await store.moreContext(question, note: note, attachments: attachments) }
                    } label: {
                        Label("I need more context", systemImage: "questionmark.bubble")
                    }
                    .help("Sends the question back to the node so it explains it better. Whatever you wrote goes along as what you’re missing.")
                } trailing: {
                    if let sendTitle, canSend {
                        SendButton(title: sendTitle, action: submit)
                    }
                }
                .animation(.snappy(duration: 0.18), value: canSend)
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

    private func optionDetail(_ option: QuestionOption) -> String? {
        let comments = draft.comments.count
        guard comments > 0 else { return option.displayDescription }
        if option.needsText || !question.isPlan {
            return String(localized: "Sends your \(comments) comments")
        }
        return String(localized: "Without your comments")
    }

    private func isProminent(_ option: QuestionOption, index: Int) -> Bool {
        guard question.isPlan else { return false }
        return draft.hasFeedback ? option.needsText : index == 0
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
        if option.needsText && text.isEmpty {
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
        } else if question.isPlan, let change = question.options.first(where: \.needsText) ?? question.options.dropFirst().first {
            answer(text, option: change.label)
        } else {
            answer(text, option: nil)
        }
    }

    private func answer(_ text: String, option: String?) {
        let attachments = draft.attachments
        resolve { await store.answer(question, text: text, option: option, attachments: attachments) }
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
    @State private var closing: CloseRequest?
    @Environment(\.searchActive) private var searchActive

    private var key: String { "r\(report.id)" }
    private var isDocument: Bool { report.kind == "document" }
    private var textAttachments: [Attachment] { isDocument ? report.attachments.filter(\.isText) : [] }
    private var otherAttachments: [Attachment] { report.attachments.filter { !textAttachments.contains($0) } }
    private var feedback: String { draft.feedback(author: store.userName) }
    private var canSend: Bool { !feedback.isEmpty || !draft.attachments.isEmpty }

    var body: some View {
        DetailScaffold {
            VStack(alignment: .leading, spacing: 22) {
                DetailHeader(style: InboxItem.report(report).style, node: report.nodeName, date: report.createdAt, title: report.title)
                MissionLine(item: .report(report))
                if report.seemsToAsk {
                    Label(Report.seemsToAskLabel, systemImage: "questionmark.bubble")
                        .font(.callout.weight(.medium))
                        .foregroundStyle(.orange)
                }
                ForEach(report.artifacts, id: \.self) { url in
                    Link(destination: url) {
                        Label(Report.openInBrowserLabel, systemImage: "safari")
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                    .help(url.absoluteString)
                }
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
                if let earlier = report.earlier, !earlier.isEmpty {
                    GroupBox {
                        VStack(alignment: .leading, spacing: 14) {
                            ForEach(earlier) { e in
                                VStack(alignment: .leading, spacing: 6) {
                                    Text(e.title)
                                        .font(.headline)
                                    if let body = e.body, !body.isEmpty {
                                        MarkdownText(text: body)
                                            .font(.callout)
                                    }
                                }
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                    } label: {
                        Label(String(localized: "Earlier in this delivery (\(earlier.count))"), systemImage: "tray.full")
                            .foregroundStyle(.secondary)
                    }
                }
            }
        } bar: {
            VStack(alignment: .leading, spacing: 12) {
                CommentsSummary(draft: draft)
                Composer(
                    placeholder: isDocument ? String(localized: "General comment (optional)…") : String(localized: "Reply to the node (optional)…"),
                    text: $draft.general,
                    focus: $focus,
                    sendTitle: nil,
                    canSend: canSend,
                    send: sendFeedback,
                    attachments: draft.attachments,
                    client: store.client
                )
                actions
            }
            .animation(.snappy(duration: 0.2), value: draft.comments.count)
        }
        .task {
            if !searchActive { focus = .pane }
            try? await Task.sleep(for: .milliseconds(450))
            armed = true
        }
        .triageKeys(focus: $focus, handle: handleKey)
        .closeConfirmation($closing) { _ in
            resolve { await store.readAndClose(report) }
        }
    }

    @ViewBuilder
    private var actions: some View {
        let node = report.closableNode(store.node(report.sessionId))
        let primary = report.primaryAction(nodeFound: node != nil)
        CardActions {
            if let node {
                Button("Read and Close Node…") {
                    closing = CloseRequest(session: node.sessionId, name: node.displayName, kind: .readAndClose)
                }
                .help("Mark as read and close the node that sent it (claude rm)")
            }
        } trailing: {
            if primary == .review {
                Button("Mark as Read", action: markRead)
                    .help("Mark as read (L)")
                Button("Approve") { review(.approved) }
                    .help("Tell the node the artifact is approved (V)")
                if canSend {
                    SendButton(title: String(localized: "Send Comments"), action: sendFeedback)
                } else {
                    Button("Rescue Comments") { review(.comments) }
                        .buttonStyle(.borderedProminent)
                        .help("The node reads your comments on the artifact and applies them (C)")
                }
            } else if canSend {
                Button("Mark as Read", action: markRead)
                    .help("Mark as read (L)")
                SendButton(title: isDocument ? String(localized: "Send Comments") : String(localized: "Reply"), action: sendFeedback)
            } else {
                Button("Mark as Read", action: markRead)
                    .buttonStyle(.borderedProminent)
                    .help("Mark as read (L)")
            }
        }
        .animation(.snappy(duration: 0.18), value: canSend)
    }

    private func handleKey(_ character: Character) -> Bool {
        guard armed, draft.editing == nil else { return false }
        switch character {
        case "r": focus = .reply
        case "l": markRead()
        case "c" where !report.artifacts.isEmpty: review(.comments)
        case "v" where !report.artifacts.isEmpty: review(.approved)
        default: return false
        }
        return true
    }

    private func sendFeedback() {
        let text = feedback
        guard !text.isEmpty || !draft.attachments.isEmpty else { return }
        let attachments = draft.attachments
        resolve { await store.reply(to: report, text: text, attachments: attachments) }
    }

    private func markRead() {
        resolve { await store.markRead(report) }
    }

    private func review(_ verdict: ReviewVerdict) {
        resolve { await store.review(report, verdict) }
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
