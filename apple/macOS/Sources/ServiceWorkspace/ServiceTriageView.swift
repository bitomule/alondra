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
        .task { await store.send(.triage(.appeared)).finish() }
        .onDisappear { store.send(.triage(.disappeared)) }
        .sheet(item: $store.scope(state: \.question, action: \.question)) { child in
            NavigationStack { ServiceQuestionView(store: child) }.frame(minWidth: 560, minHeight: 480)
        }
        .sheet(item: $store.scope(state: \.document, action: \.document)) { child in
            NavigationStack {
                ServiceDocumentView(store: child, markRead: {
                    store.send(.triage(.hide(child.subject.id)))
                    store.send(.document(.dismiss))
                })
            }.frame(minWidth: 640, minHeight: 520)
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
    @FocusState private var focus: DetailField?
    @Environment(\.searchActive) private var searchActive
    var body: some View {
        DetailScaffold {
            VStack(alignment: .leading, spacing: 22) {
                DetailHeader(style: InboxItem.question(store.question).style, node: store.question.nodeName,
                    date: store.question.askedAt, title: store.question.title)
                MissionLine(item: .question(store.question))
                if let detail = store.question.readableDetail, !detail.isEmpty {
                    MarkdownText(text: store.question.origin == "permission" ? "```\n\(detail)\n```" : detail)
                }
                ForEach(store.question.links, id: \.self) { link in
                    if let url = URL(string: link.url) {
                        Link(destination: url) { Label(link.title ?? link.url, systemImage: "arrow.up.right.square").lineLimit(1) }.font(.callout)
                    }
                }
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
                        submit: { store.send(.answerTapped) })
                }
                AttachmentPicker(store: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
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
            }.disabled(!store.attachmentPicker.allReady || store.isSubmitting || store.needsReconciliation || store.completed)
        }
        .onAppear { if !searchActive { focus = store.question.options.isEmpty ? .reply : .pane } }
        .onChange(of: store.replyFocusRequest) { _, _ in focus = .reply }
    }
}

struct ServiceDocumentView: View {
    let store: StoreOf<DocumentReviewFeature>
    var markRead: (() -> Void)?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        Form {
            Section {
                if store.isLoading { ProgressView("Loading document…") }
                if let error = store.loadError {
                    Text(error).foregroundStyle(.red)
                    Button("Retry") { store.send(.appeared) }
                }
                if let text = store.sourceText { ServiceDocumentBody(store: store, blocks: MarkdownParser.blocks(from: text)) }
            }
            if case .report(let report) = store.subject {
                ForEach(report.earlier ?? []) { earlier in
                    Section(earlier.title) { ServiceReportContext(report: earlier) }
                }
            }
            Section("Feedback") {
                if case .question(let question) = store.subject {
                    ForEach(question.options, id: \.label) { option in
                        Button(option.label) { store.send(.optionSelected(option.label)) }
                    }
                    if question.isPlan {
                        Button("Approve") { store.send(.optionSelected(AnswerOption.approve)) }
                        Button("Request changes") { store.send(.optionSelected(AnswerOption.requestChanges)) }
                    }
                    if let option = store.draft.selectedOption { Label(option, systemImage: "checkmark.circle") }
                }
                TextField("General feedback", text: Binding(get: { store.draft.general }, set: { store.send(.generalChanged($0)) }), axis: .vertical)
                    .lineLimit(4...12)
                Button(store.isSubmitting ? "Sending…" : "Send feedback") { store.send(.sendFeedbackTapped(author: "David")) }
                if case .report(let report) = store.subject {
                    if !report.artifacts.isEmpty {
                        Button("Approve artifact") { store.send(.reviewTapped(.approved)) }
                    }
                    if let markRead { Button("Mark read", action: markRead) }
                }
            }.disabled(!store.attachmentPicker.allReady || store.isLoading || store.loadError != nil || store.isSubmitting || store.needsReconciliation || store.completed)
            if store.feedbackReadRetryAvailable {
                Section {
                    Button(store.isRetryingFeedbackRead ? "Checking…" : "Retry marking report read") { store.send(.retryFeedbackRead) }
                        .disabled(store.isRetryingFeedbackRead)
                    Text("Your feedback has already been delivered. This only marks the report read.").font(.caption)
                }
            }
            AttachmentPicker(store: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
                .disabled(store.isSubmitting || store.needsReconciliation || store.completed)
            ServiceWorkNotice(notice: store.notice, conflict: store.conflict)
        }
        .navigationTitle("Document")
        .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } } }
        .task { await store.send(.appeared).finish() }
        .onDisappear { store.send(.disappeared) }
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

private struct ServiceDocumentBody: View {
    let store: StoreOf<DocumentReviewFeature>
    let blocks: [MarkdownBlock]

    var body: some View {
        ForEach(blocks) { block in
            VStack(alignment: .leading, spacing: 8) {
                BlockView(block: block).textSelection(.enabled)
                TextField("Comment on this passage", text: Binding(
                    get: { store.draft.comments.first(where: { $0.anchor == block.draftKey })?.text ?? "" },
                    set: { store.send(.commentChanged(anchor: block.draftKey, quote: block.plainText, text: $0)) }
                ), axis: .vertical)
                .font(.callout)
                .disabled(store.isSubmitting || store.needsReconciliation || store.completed)
            }
        }
    }
}

private struct ServiceReportContext: View {
    let report: Report
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let body = report.body {
                MarkdownText(text: body)
            }
        }.textSelection(.enabled)
    }
}
