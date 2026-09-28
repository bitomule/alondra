import AlondraAttachmentsFeature
import AlondraWorkspaceFeature
import AlondraQuestionsFeature
import AlondraShared
import AlondraTriageFeature
import ComposableArchitecture
import SwiftUI

struct ServiceTriageView: View {
    @Bindable var store: StoreOf<ServiceTriageFeature>

    var body: some View {
        List {
            Section {
                Label("\(store.triage.workingNodes) working · \(store.triage.waitingNodes) waiting", systemImage: "desktopcomputer")
            }
            if !store.triage.hasSnapshot {
                ProgressView("Loading inbox…")
            } else if store.triage.queue.isEmpty {
                ContentUnavailableView("All caught up", systemImage: "checkmark.circle", description: Text("New questions and reports will appear here."))
            } else {
                ForEach(store.triage.queue) { item in
                    ServiceInboxRow(item: item, pending: store.triage.pendingIDs.contains(item.id),
                        open: { store.send(.open(item.id)) },
                        skip: { store.send(.triage(.skip(item.id))) },
                        hide: { store.send(.triage(.hide(item.id))) })
                }
            }
            if let notice = store.triage.notice {
                Section {
                    Text(notice).foregroundStyle(.red)
                    Button("Dismiss") { store.send(.triage(.dismissNotice)) }
                }
            }
        }
        .navigationTitle("Triage")
        .onDisappear { store.send(.presentationDisappeared) }
        .sheet(item: $store.scope(state: \.question, action: \.question)) { child in
            NavigationStack { ServiceQuestionView(store: child) }.frame(minWidth: 560, minHeight: 480)
        }
        .sheet(isPresented: Binding(get: { store.documentID != nil }, set: { if !$0 { store.send(.documentDismissed) } })) {
            if let child = store.scope(state: \.documents, action: \.documents).first(where: { $0.subject.id == store.documentID }) {
            NavigationStack {
                ServiceDocumentView(store: child, markRead: {
                    store.send(.triage(.hide(child.subject.id)))
                    store.send(.documentDismissed)
                })
            }.frame(minWidth: 640, minHeight: 520)
            }
        }
    }
}

private struct ServiceInboxRow: View {
    let item: InboxItem
    let pending: Bool
    let open: () -> Void
    let skip: () -> Void
    let hide: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button(action: open) {
                VStack(alignment: .leading, spacing: 5) {
                    Text(item.title).font(.headline)
                    if let node = item.nodeName { Text(node).font(.caption).foregroundStyle(.secondary) }
                }.frame(maxWidth: .infinity, alignment: .leading)
            }.buttonStyle(.plain)
            HStack {
                Button("Later", action: skip)
                Spacer()
                switch item {
                case .question: EmptyView()
                case .report: Button("Mark read", action: hide)
                case .pullRequest: Button("Dismiss", action: hide)
                }
            }.font(.subheadline).buttonStyle(.borderless)
        }.disabled(pending)
    }
}

struct ServiceQuestionView: View {
    let store: StoreOf<QuestionFeature>
    var taskTitle: String? = nil
    @FocusState private var focus: DetailField?
    @Environment(\.searchActive) private var searchActive
    var body: some View {
        DetailScaffold {
            VStack(alignment: .leading, spacing: 22) {
                DetailHeader(style: InboxItem.question(store.question).style, node: store.question.nodeName,
                    date: store.question.askedAt, title: store.question.title, task: taskTitle)
                MissionLine(item: .question(store.question))
                if let detail = store.question.readableDetail, !detail.isEmpty {
                    MarkdownText(text: store.question.origin == "permission" ? "```\n\(detail)\n```" : detail)
                }
                ServiceLinksRow(links: store.question.links, attachments: store.question.attachments,
                    store: store.scope(state: \.downloads, action: \.downloads))
                ForEach(store.question.aboutReports) { report in
                    GroupBox {
                        VStack(alignment: .leading, spacing: 8) {
                            Text(report.title).font(.headline)
                            if let body = report.body, !body.isEmpty { MarkdownText(text: body).font(.callout) }
                        }.frame(maxWidth: .infinity, alignment: .leading)
                    } label: {
                        Label(String(localized: "About report #\(report.id)"), systemImage: "doc.text").foregroundStyle(.secondary)
                    }
                }
                if let context = store.question.nodeContext, !context.isEmpty {
                    DisclosureGroup("What the node said last") {
                        MarkdownText(text: context).font(.callout).foregroundStyle(.secondary).padding(.top, 6)
                    }.foregroundStyle(.secondary)
                }
            }
        } bar: {
            VStack(alignment: .leading, spacing: 12) {
                if !store.question.options.isEmpty {
                    QuestionOptions(options: store.question.options, selected: store.draft.selectedOption) { option in
                        store.send(.optionTapped(option.label))
                    }
                }
                if !store.question.isTerminalMenu {
                    ServiceComposer(placeholder: store.question.options.isEmpty ? String(localized: "Your answer…") : String(localized: "Or answer in your own words…"),
                        text: Binding(get: { store.draft.text }, set: { store.send(.textChanged($0)) }), focus: $focus,
                        sendTitle: nil, canSend: !store.draft.text.isEmpty || !store.attachmentPicker.entries.isEmpty,
                        submit: { store.send(.answerTapped) },
                        escape: { store.send(.optionSelected(nil)) },
                        attachmentsStore: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
                }
                CardActions {
                    if !store.question.isTerminalMenu {
                        Button { store.send(.moreContextTapped) } label: { Label("I need more context", systemImage: "questionmark.bubble") }
                            .help("Sends the question back to the node so it explains it better. Whatever you wrote goes along as what you’re missing.")
                    }
                } trailing: {
                    if store.draft.selectedOption != nil || !store.draft.text.isEmpty || !store.draft.attachmentIDs.isEmpty {
                        SendButton(title: store.draft.selectedOption.map(AnswerOption.displayLabel) ?? String(localized: "Reply")) { store.send(.answerTapped) }
                    }
                }
                ServiceWorkNotice(notice: store.notice, conflict: store.conflict)
            }.disabled(!store.attachmentPicker.canPrepareSubmission || store.pendingSubmission != nil || store.isSubmitting || store.needsReconciliation || store.completed)
        }
        .attachmentDownloadPresentation(store: store.scope(state: \.downloads, action: \.downloads))
        .onAppear { if !searchActive { focus = store.question.options.isEmpty ? .reply : .pane } }
        .onChange(of: store.replyFocusRequest) { _, _ in focus = .reply }
        .onAppear { store.send(.keyboardAppeared) }
        .onDisappear { store.send(.disappeared); store.send(.keyboardDisappeared) }
        .triageKeys(focus: $focus) { character in
            guard store.keyboardReady else { return false }
            if character == "r" { focus = .reply; return true }
            guard let number = character.wholeNumberValue, number >= 1, number <= store.question.options.count else { return false }
            store.send(.optionTapped(store.question.options[number - 1].label))
            return true
        }
    }
}

struct ServiceDocumentView: View {
    let store: StoreOf<DocumentReviewFeature>
    var taskTitle: String? = nil
    var markRead: (() -> Void)?
    @FocusState private var focus: DetailField?
    @Environment(\.searchActive) private var searchActive
    private var item: InboxItem {
        switch store.subject { case .question(let q): .question(q); case .report(let r): .report(r) }
    }
    var body: some View {
        DetailScaffold {
            VStack(alignment: .leading, spacing: 22) {
                header
                MissionLine(item: item)
                if case .report(let report) = store.subject {
                    if report.seemsToAsk {
                        Label(Report.seemsToAskLabel, systemImage: "questionmark.bubble")
                            .font(.callout.weight(.medium)).foregroundStyle(.orange)
                    }
                    ForEach(report.artifacts, id: \.self) { url in
                        Link(destination: url) { Label(Report.openInBrowserLabel, systemImage: "safari") }
                            .buttonStyle(.borderedProminent).controlSize(.large).help(url.absoluteString)
                    }
                }
                if let error = store.loadError {
                    if !store.sections.contains(where: { $0.error != nil }) { Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.secondary) }
                    Button("Retry") { store.send(.appeared) }
                }
                ForEach(store.sections) { section in
                    switch section.source {
                    case .inline:
                        if !section.text.isEmpty {
                            if isReviewable {
                                ServiceReviewableDocument(text: section.text, anchorPrefix: bodyPrefix, store: store, focus: $focus)
                            } else if case .question(let question) = store.subject, question.origin == "permission" {
                                MarkdownText(text: "```\n\(section.text)\n```")
                            } else { MarkdownText(text: section.text) }
                        }
                    case .stored(let name, let path):
                        ServiceAttachmentDocument(name: name, path: path, section: section, store: store, focus: $focus)
                    }
                }
                context
            }
        } bar: {
            VStack(alignment: .leading, spacing: 12) {
                ServiceCommentsSummary(comments: store.draft.comments) { comment in
                    store.send(.commentChanged(anchor: comment.anchor, quote: comment.quote, text: ""))
                }
                if case .question(let question) = store.subject, !question.options.isEmpty {
                    QuestionOptions(options: question.options, selected: store.draft.selectedOption,
                        detail: { option in
                            guard !store.draft.comments.isEmpty else { return option.displayDescription }
                            return option.needsText || !question.isPlan
                                ? String(localized: "Sends your \(store.draft.comments.count) comments")
                                : String(localized: "Without your comments")
                        }, prominent: { option, index in question.isPlan && (store.hasFeedback ? option.needsText : index == 0) },
                        choose: choose)
                }
                ServiceComposer(placeholder: placeholder,
                    text: Binding(get: { store.draft.general }, set: { store.send(.generalChanged($0)) }), focus: $focus,
                    sendTitle: nil, canSend: store.hasFeedback,
                    submit: { submit() },
                    escape: { store.send(.optionSelected(nil)) },
                    attachmentsStore: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
                actions
                if store.feedbackReadRetryAvailable {
                    Button(store.isRetryingFeedbackRead ? "Checking…" : "Retry marking report read") { store.send(.retryFeedbackRead) }
                        .disabled(store.isRetryingFeedbackRead)
                    Text("Your feedback has already been delivered. This only marks the report read.").font(.caption)
                }
                ServiceWorkNotice(notice: store.notice, conflict: store.conflict)
                if let error = store.downloads.error { Text(error).foregroundStyle(.red) }
            }
            .disabled(store.isSubmitting || store.pendingSubmission != nil || store.needsReconciliation || store.completed)
            .animation(.snappy(duration: 0.2), value: store.draft.comments.count)
        }
        .task { await store.send(.appeared).finish() }
        .onDisappear { store.send(.disappeared) }
        .onAppear {
            if !searchActive {
                if case .question(let question) = store.subject {
                    focus = question.options.isEmpty && !question.isPlan ? .reply : .pane
                } else { focus = .pane }
            }
        }
        .attachmentDownloadPresentation(store: store.scope(state: \.downloads, action: \.downloads))
        .onChange(of: store.generalFocusRequest) { _, _ in focus = .reply }
        .onAppear { store.send(.keyboardAppeared) }
        .onDisappear { store.send(.keyboardDisappeared) }
        .triageKeys(focus: $focus) { character in
            guard store.keyboardReady, store.composingAnchor == nil else { return false }
            if character == "r" { focus = .reply; return true }
            switch store.subject {
            case .question(let question):
                guard let number = character.wholeNumberValue, number >= 1, number <= question.options.count else { return false }
                choose(question.options[number - 1])
                return true
            case .report(let report):
                switch character {
                case "l": markRead?()
                case "c" where !report.artifacts.isEmpty: store.send(.reviewTapped(.comments))
                case "v" where !report.artifacts.isEmpty: store.send(.reviewTapped(.approved))
                default: return false
                }
                return true
            }
        }
    }
    private func choose(_ option: QuestionOption) {
        let focusRequest = store.generalFocusRequest
        store.send(.chooseReviewOption(option, author: store.feedbackAuthor))
        focus = store.generalFocusRequest == focusRequest ? .pane : .reply
    }

    private func submit() {
        focus = .pane
        store.send(.submitFeedback(author: store.feedbackAuthor))
    }

    @ViewBuilder private var header: some View {
        switch store.subject {
        case .question(let q): DetailHeader(style: item.style, node: q.nodeName, date: q.askedAt, title: q.title, task: taskTitle)
        case .report(let r): DetailHeader(style: item.style, node: r.nodeName, date: r.createdAt, title: r.title, task: taskTitle)
        }
    }
    private var bodyPrefix: String {
        switch store.subject { case .question: "plan"; case .report: "cuerpo" }
    }
    private var isReviewable: Bool {
        switch store.subject { case .question(let q): q.isPlan; case .report(let r): r.kind == "document" }
    }
    private var placeholder: String {
        if let option = store.draft.selectedOption { return String(localized: "\(AnswerOption.displayLabel(option)): write why…") }
        switch store.subject {
        case .question(let q): return q.isPlan ? String(localized: "General comment on the plan (optional)…") : q.options.isEmpty ? String(localized: "Your answer…") : String(localized: "Or answer in your own words…")
        case .report(let r): return r.kind == "document" ? String(localized: "General comment (optional)…") : String(localized: "Reply to the node (optional)…")
        }
    }
    @ViewBuilder private var context: some View {
        switch store.subject {
        case .question(let q):
            ServiceLinksRow(links: q.links, attachments: q.attachments.filter { !$0.isText }, store: store.scope(state: \.downloads, action: \.downloads))
            ForEach(q.aboutReports) { report in
                GroupBox {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(report.title).font(.headline)
                        if let body = report.body, !body.isEmpty { MarkdownText(text: body).font(.callout) }
                    }.frame(maxWidth: .infinity, alignment: .leading)
                } label: { Label(String(localized: "About report #\(report.id)"), systemImage: "doc.text").foregroundStyle(.secondary) }
            }
            if let text = q.nodeContext?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty {
                DisclosureGroup("What the node said last") { MarkdownText(text: text).font(.callout).foregroundStyle(.secondary).padding(.top, 6) }.foregroundStyle(.secondary)
            }
        case .report(let r):
            ServiceLinksRow(links: r.links, attachments: r.attachments.filter { r.kind != "document" || !$0.isText }, store: store.scope(state: \.downloads, action: \.downloads))
            if let earlier = r.earlier, !earlier.isEmpty {
                GroupBox {
                    VStack(alignment: .leading, spacing: 14) {
                        ForEach(earlier) { report in
                            VStack(alignment: .leading, spacing: 6) {
                                Text(report.title).font(.headline)
                                if let body = report.body, !body.isEmpty { MarkdownText(text: body).font(.callout) }
                            }
                        }
                    }.frame(maxWidth: .infinity, alignment: .leading)
                } label: { Label(String(localized: "Earlier in this delivery (\(earlier.count))"), systemImage: "tray.full").foregroundStyle(.secondary) }
            }
        }
    }
    @ViewBuilder private var actions: some View {
        if case .report(let report) = store.subject {
            CardActions {} trailing: {
                if report.primaryAction() == .review {
                    Button("Mark as Read") { markRead?() }.help("Mark as read (L)")
                    Group {
                        Button("Approve") { store.send(.reviewTapped(.approved)) }.help("Tell the node the artifact is approved (V)")
                        if store.hasFeedback {
                            SendButton(title: String(localized: "Send Comments")) { submit() }
                        } else {
                            Button("Rescue Comments") { store.send(.reviewTapped(.comments)) }.buttonStyle(.borderedProminent)
                                .help("The node reads your comments on the artifact and applies them (C)")
                        }
                    }.disabled(!store.canReview)
                } else if store.hasFeedback {
                    Button("Mark as Read") { markRead?() }.help("Mark as read (L)")
                    SendButton(title: report.kind == "document" ? String(localized: "Send Comments") : String(localized: "Reply")) { submit() }
                        .disabled(!store.canReview)
                } else {
                    Button("Mark as Read") { markRead?() }.buttonStyle(.borderedProminent).help("Mark as read (L)")
                }
            }.animation(.snappy(duration: 0.18), value: store.hasFeedback)
        } else if case .question(let q) = store.subject {
            CardActions {
                Button { focus = .pane; store.send(.moreContextTapped(author: store.feedbackAuthor)) } label: { Label("I need more context", systemImage: "questionmark.bubble") }
                    .help("Sends the question back to the node so it explains it better. Whatever you wrote goes along as what you’re missing.")
                    .disabled(!store.canReview)
            } trailing: {
                if store.hasFeedback, store.draft.selectedOption != nil || !q.isPlan {
                    SendButton(title: store.draft.selectedOption.map(AnswerOption.displayLabel) ?? String(localized: "Reply")) { submit() }
                        .disabled(!store.canReview)
                }
            }
        }
    }
}

private struct ServiceWorkNotice: View {
    let notice: String?
    let conflict: String?
    var body: some View {
        Section {
            if let conflict { Text(conflict).foregroundStyle(.orange) }
            if let notice { Text(notice).foregroundStyle(.red) }
        }
    }
}
