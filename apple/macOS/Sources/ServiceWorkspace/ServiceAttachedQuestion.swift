import AlondraQuestionsFeature
import ComposableArchitecture
import AlondraShared
import SwiftUI

/// A question the node asks about the PR it sits in, answered right there in the PR's detail.
struct ServiceAttachedQuestion: View {
    let store: StoreOf<QuestionFeature>
    let pullRequestURL: String
    @FocusState private var focus: DetailField?
    private var question: Question { store.question }
    private var style: KindStyle { InboxItem.question(question).style }
    private var pendingOption: String? { store.draft.selectedOption }
    private var placeholder: String {
        if let pendingOption { return String(localized: "\(AnswerOption.displayLabel(pendingOption)): write why…") }
        return question.options.isEmpty ? String(localized: "Your answer…") : String(localized: "Or answer in your own words…")
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: style.symbol)
                    .foregroundStyle(style.tint)
                    .accessibilityHidden(true)
                Text(question.title)
                    .textSelection(.enabled)
            }
            .font(.headline)
            if let detail = question.readableDetail, !detail.isEmpty {
                MarkdownText(text: question.origin == "permission" ? "```\n\(detail)\n```" : detail)
            }
            ServiceLinksRow(links: question.links.filter { $0.url != pullRequestURL }, attachments: question.attachments, store: store.scope(state: \.downloads, action: \.downloads))
            if !question.options.isEmpty {
                QuestionOptions(options: question.options, selected: pendingOption, showsKeys: false, choose: {
                    focus = nil
                    store.send(.optionTapped($0.label))
                })
            }
            ServiceComposer(
                placeholder: placeholder,
                text: Binding(get: { store.draft.text }, set: { store.send(.textChanged($0)) }),
                focus: $focus,
                sendTitle: pendingOption.map(AnswerOption.displayLabel) ?? String(localized: "Reply"),
                canSend: !store.draft.text.isEmpty || !store.attachmentPicker.entries.isEmpty,
                submit: {
                    focus = nil
                    store.send(.answerTapped)
                },
                escape: { store.send(.optionSelected(nil)) },
                attachmentsStore: store.scope(state: \.attachmentPicker, action: \.attachmentPicker)
            )
            Button {
                focus = nil
                store.send(.moreContextTapped)
            } label: {
                Label("I need more context", systemImage: "questionmark.bubble")
            }
            .buttonStyle(.link)
            .font(.callout)
        }
        .attachmentDownloadPresentation(store: store.scope(state: \.downloads, action: \.downloads))
        .disabled(store.isSubmitting || store.pendingSubmission != nil || store.needsReconciliation || store.completed)
        .onDisappear { store.send(.disappeared) }
        .onChange(of: store.replyFocusRequest) { _, _ in focus = .reply }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(style.tint.opacity(0.08), in: .rect(cornerRadius: 12))
        .overlay {
            RoundedRectangle(cornerRadius: 12)
                .strokeBorder(style.tint.opacity(0.3))
        }
    }

}
