import AlondraAttachmentsFeature
import AlondraWorkspaceFeature
import AlondraQuestionsFeature
import AlondraShared
import AlondraTriageFeature
import ComposableArchitecture
import SwiftUI

struct ServiceTriageView: View {
    @Bindable var store: StoreOf<ServiceTriageFeature>

    private var working: Int { store.nodes.filter { $0.state == .working }.count }
    private var waiting: Int {
        store.nodes.filter { [.waitingAnswer, .yourTurn, .waitingMerge, .waitingMachine].contains($0.state) }.count
    }

    var body: some View {
        List {
            Section {
                Label("\(working) working · \(waiting) waiting", systemImage: "desktopcomputer")
            }
            if !store.triage.hasSnapshot {
                ProgressView("Loading inbox…")
            } else if store.triage.queue.isEmpty {
                ContentUnavailableView("All caught up", systemImage: "checkmark.circle", description: Text("New questions and reports will appear here."))
            } else {
                ForEach(store.triage.queue) { item in
                    ServiceInboxRow(item: item, agent: store.workspace?.agentName(of: item),
                        pending: store.triage.pendingIDs.contains(item.id),
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
    let item: DomainV2.InboxItem
    let agent: String?
    let pending: Bool
    let open: () -> Void
    let skip: () -> Void
    let hide: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button(action: open) {
                VStack(alignment: .leading, spacing: 5) {
                    Text(item.title).font(.headline)
                    if let agent { Text(agent).font(.caption).foregroundStyle(.secondary) }
                }.frame(maxWidth: .infinity, alignment: .leading)
            }.buttonStyle(.plain)
            HStack {
                Button("Later", action: skip)
                Spacer()
                if item.availableActions.contains(where: { $0.actionKind == .markRead && $0.enabled }) {
                    Button("Mark read", action: hide)
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
    private var item: DomainV2.InboxItem { store.question.item }
    var body: some View {
        DetailScaffold {
            VStack(alignment: .leading, spacing: 22) {
                DetailHeader(style: item.style, node: store.workspace.agentName(of: item),
                    date: store.workspace.createdAt(of: item), title: store.question.title, task: taskTitle)
                MissionLine(mission: store.workspace.mission(of: item))
                if !store.question.body.isEmpty {
                    MarkdownText(text: store.question.body)
                }
                ServiceLinksRow(attachments: store.question.attachments, store: store.scope(state: \.downloads, action: \.downloads))
                if let context = store.workspace.runtime?.node(item.agentId)?.lastSaid {
                    DisclosureGroup("What the node said last") {
                        MarkdownText(text: context).font(.callout).foregroundStyle(.secondary).padding(.top, 6)
                    }.foregroundStyle(.secondary)
                }
            }
        } bar: {
            VStack(alignment: .leading, spacing: 12) {
                if !store.question.options.isEmpty {
                    QuestionOptions(options: store.question.options, selected: store.draft.selectedOption) { option in
                        store.send(.optionTapped(option.id))
                    }
                }
                ServiceComposer(placeholder: store.question.options.isEmpty ? String(localized: "Your answer…") : String(localized: "Or answer in your own words…"),
                    text: Binding(get: { store.draft.text }, set: { store.send(.textChanged($0)) }), focus: $focus,
                    sendTitle: nil, canSend: !store.draft.text.isEmpty || !store.attachmentPicker.entries.isEmpty,
                    submit: { store.send(.answerTapped) },
                    escape: { store.send(.optionSelected(nil)) },
                    attachmentsStore: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
                CardActions {
                    Button { store.send(.moreContextTapped) } label: { Label("I need more context", systemImage: "questionmark.bubble") }
                        .help("Sends the question back to the node so it explains it better. Whatever you wrote goes along as what you’re missing.")
                } trailing: {
                    if store.draft.selectedOption != nil || !store.draft.text.isEmpty || !store.attachmentPicker.entries.isEmpty {
                        SendButton(title: store.selectedOption?.displayLabel ?? String(localized: "Reply")) { store.send(.answerTapped) }
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
            store.send(.optionTapped(store.question.options[number - 1].id))
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
    private var item: DomainV2.InboxItem { store.subject.item }
    private var question: DomainQuestion? { store.subject.question }
    private var canMarkRead: Bool { store.subject.action(.markRead)?.enabled == true }
    var body: some View {
        DetailScaffold {
            VStack(alignment: .leading, spacing: 22) {
                DetailHeader(style: item.style, node: store.workspace.agentName(of: item),
                    date: store.workspace.createdAt(of: item), title: store.subject.title, task: taskTitle)
                MissionLine(mission: store.workspace.mission(of: item))
                if question == nil {
                    ForEach(Artifacts.urls(in: store.subject.interaction.body), id: \.self) { url in
                        Link(destination: url) { Label(Artifacts.openInBrowserLabel, systemImage: "safari") }
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
                            if store.subject.isDelivery {
                                ServiceReviewableDocument(text: section.text, anchorPrefix: bodyPrefix, store: store, focus: $focus)
                            } else { MarkdownText(text: section.text) }
                        }
                    case .revision(let name, let id, _):
                        ServiceAttachmentDocument(name: name, anchor: id.rawValue, section: section, store: store, focus: $focus)
                    case .attachment(let attachment):
                        ServiceAttachmentDocument(name: attachment.name, anchor: attachment.id, attachment: attachment, section: section, store: store, focus: $focus)
                    }
                }
                ServiceLinksRow(attachments: store.subject.attachments.filter { !$0.isText }, store: store.scope(state: \.downloads, action: \.downloads))
            }
        } bar: {
            VStack(alignment: .leading, spacing: 12) {
                ServiceCommentsSummary(comments: store.draft.comments) { comment in
                    store.send(.commentChanged(anchor: comment.anchor, quote: comment.quote, text: ""))
                }
                if let question, !question.options.isEmpty {
                    QuestionOptions(options: question.options,
                        detail: { option in
                            guard !store.draft.comments.isEmpty else { return option.description }
                            return option.inputs.contains { $0.kind == .text }
                                ? String(localized: "Sends your \(store.draft.comments.count) comments")
                                : String(localized: "Without your comments")
                        },
                        choose: choose)
                }
                ServiceComposer(placeholder: placeholder,
                    text: Binding(get: { store.draft.general }, set: { store.send(.generalChanged($0)) }), focus: $focus,
                    sendTitle: nil, canSend: store.hasFeedback,
                    submit: { submit() },
                    attachmentsStore: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
                actions
                ServiceWorkNotice(notice: store.notice, conflict: store.conflict)
                if let error = store.downloads.error { Text(error).foregroundStyle(.red) }
            }
            .disabled(store.isSubmitting || store.pendingReview != nil || store.needsReconciliation || store.completed)
            .animation(.snappy(duration: 0.2), value: store.draft.comments.count)
        }
        .task { await store.send(.appeared).finish() }
        .onDisappear { store.send(.disappeared) }
        .onAppear {
            if !searchActive {
                if let question {
                    focus = question.options.isEmpty ? .reply : .pane
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
            if let question {
                guard let number = character.wholeNumberValue, number >= 1, number <= question.options.count else { return false }
                choose(question.options[number - 1])
                return true
            }
            switch character {
            case "l" where canMarkRead: markRead?()
            case "c" where store.subject.action(.submitFeedback) != nil: store.send(.reviewTapped(.comments))
            case "v" where store.subject.action(.acceptDelivery) != nil: store.send(.reviewTapped(.approved))
            default: return false
            }
            return true
        }
    }
    private func choose(_ option: DomainV2.QuestionOption) {
        let focusRequest = store.generalFocusRequest
        store.send(.chooseReviewOption(option.id, author: store.feedbackAuthor))
        focus = store.generalFocusRequest == focusRequest ? .pane : .reply
    }

    private func submit() {
        focus = .pane
        store.send(.sendFeedbackTapped(author: store.feedbackAuthor))
    }

    private var bodyPrefix: String { question == nil ? "cuerpo" : "plan" }
    private var placeholder: String {
        if let question { return question.options.isEmpty ? String(localized: "Your answer…") : String(localized: "Or answer in your own words…") }
        return store.subject.isDelivery ? String(localized: "General comment (optional)…") : String(localized: "Reply to the node (optional)…")
    }
    @ViewBuilder private var actions: some View {
        if question != nil {
            CardActions {
                Button { focus = .pane; store.send(.moreContextTapped(author: store.feedbackAuthor)) } label: { Label("I need more context", systemImage: "questionmark.bubble") }
                    .help("Sends the question back to the node so it explains it better. Whatever you wrote goes along as what you’re missing.")
                    .disabled(!store.canReview)
            } trailing: {}
        } else if store.subject.isDelivery {
            CardActions {} trailing: {
                if canMarkRead { Button("Mark as Read") { markRead?() }.help("Mark as read (L)") }
                Group {
                    if store.subject.action(.acceptDelivery) != nil {
                        Button("Approve") { store.send(.reviewTapped(.approved)) }.help("Tell the node the artifact is approved (V)")
                            .disabled(!store.canAccept)
                    }
                    if store.subject.action(.submitFeedback) != nil {
                        if store.hasFeedback {
                            SendButton(title: String(localized: "Send Comments")) { submit() }
                        } else {
                            Button("Rescue Comments") { store.send(.reviewTapped(.comments)) }.buttonStyle(.borderedProminent)
                                .help("The node reads your comments on the artifact and applies them (C)")
                        }
                    }
                }.disabled(!store.canReview)
            }.animation(.snappy(duration: 0.18), value: store.hasFeedback)
        } else {
            CardActions {} trailing: {
                if store.hasFeedback {
                    if canMarkRead { Button("Mark as Read") { markRead?() }.help("Mark as read (L)") }
                    SendButton(title: String(localized: "Reply")) { submit() }
                        .disabled(!store.canReply)
                } else if canMarkRead {
                    Button("Mark as Read") { markRead?() }.buttonStyle(.borderedProminent).help("Mark as read (L)")
                }
            }.animation(.snappy(duration: 0.18), value: store.hasFeedback)
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
