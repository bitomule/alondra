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
            NavigationStack { ServiceQuestionView(store: child) }
        }
        .sheet(item: $store.scope(state: \.document, action: \.document)) { child in
            NavigationStack {
                ServiceDocumentView(store: child, markRead: {
                    store.send(.triage(.hide(child.subject.id)))
                    store.send(.document(.dismiss))
                })
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
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        Form {
            Section {
                Text(store.question.title).font(.headline)
                if let detail = store.question.readableDetail { Text(detail).textSelection(.enabled) }
            }
            ForEach(store.question.aboutReports) { report in
                Section(report.title) { ServiceReportContext(report: report) }
            }
            Section("Answer") {
                ForEach(store.question.options, id: \.label) { option in
                    Button {
                        store.send(.optionSelected(option.label))
                    } label: {
                        HStack {
                            VStack(alignment: .leading) {
                                Text(option.label)
                                if let detail = option.description { Text(detail).font(.caption).foregroundStyle(.secondary) }
                            }
                            Spacer()
                            if store.draft.selectedOption == option.label { Image(systemName: "checkmark") }
                        }
                    }
                }
                if !store.question.isTerminalMenu {
                    TextField("Write your answer", text: Binding(get: { store.draft.text }, set: { store.send(.textChanged($0)) }), axis: .vertical)
                        .lineLimit(4...12)
                }
                Button(store.isSubmitting ? "Sending…" : "Send answer") { store.send(.answerTapped) }
                if !store.question.isTerminalMenu {
                    Button("Ask for more context") { store.send(.moreContextTapped) }
                }
            }.disabled(store.isSubmitting || store.needsReconciliation || store.completed)
            ServiceWorkNotice(notice: store.notice, conflict: store.conflict)
        }
        .navigationTitle(store.question.isAction ? "Action" : "Question")
        .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } } }
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
            }.disabled(store.isLoading || store.loadError != nil || store.isSubmitting || store.needsReconciliation || store.completed)
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
                MarkdownBlockView(block: block).textSelection(.enabled)
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
                ForEach(MarkdownParser.segments(from: body)) { segment in
                    MarkdownSegmentView(segment: segment)
                }
            }
        }.textSelection(.enabled)
    }
}
