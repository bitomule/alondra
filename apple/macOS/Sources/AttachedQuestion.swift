import AlondraShared
import SwiftUI

/// A question the node asks about the PR it sits in, answered right there in the PR's detail.
struct AttachedQuestion: View {
    let question: Question
    let pullRequestURL: String
    let store: FleetStore
    let drafts: Drafts
    @Bindable var draft: ReviewDraft
    @FocusState private var focus: DetailField?
    @State private var pendingOption: String?

    private var key: String { InboxItem.question(question).id }
    private var style: KindStyle { InboxItem.question(question).style }
    private var feedback: String { draft.feedback(author: store.userName) }

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
            LinksRow(links: question.links.filter { $0.url != pullRequestURL }, attachments: question.attachments)
            if !question.options.isEmpty {
                QuestionOptions(options: question.options, selected: pendingOption, showsKeys: false, choose: choose)
            }
            Composer(
                placeholder: placeholder,
                text: $draft.general,
                focus: $focus,
                sendTitle: pendingOption.map(AnswerOption.displayLabel) ?? String(localized: "Reply"),
                canSend: !feedback.isEmpty,
                send: submit,
                escape: { pendingOption = nil }
            )
            Button {
                let note = feedback
                resolve { await store.moreContext(question, note: note) }
            } label: {
                Label("I need more context", systemImage: "questionmark.bubble")
            }
            .buttonStyle(.link)
            .font(.callout)
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(style.tint.opacity(0.08), in: .rect(cornerRadius: 12))
        .overlay {
            RoundedRectangle(cornerRadius: 12)
                .strokeBorder(style.tint.opacity(0.3))
        }
    }

    private var placeholder: String {
        if let pendingOption { return String(localized: "\(AnswerOption.displayLabel(pendingOption)): write why…") }
        return question.options.isEmpty ? String(localized: "Your answer…") : String(localized: "Or answer in your own words…")
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
        answer(text, option: pendingOption)
    }

    private func answer(_ text: String, option: String?) {
        resolve { await store.answer(question, text: text, option: option) }
    }

    private func resolve(_ action: @escaping @MainActor () async -> Bool) {
        focus = nil
        let drafts = drafts
        let key = key
        Task {
            if await action() { drafts.discard(key) }
        }
    }
}
