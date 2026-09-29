import AlondraShared
import SwiftUI
import AlondraQuestionsFeature
import ComposableArchitecture

/// A question the node asks about the PR it sits in, answered right there without leaving the PR.
struct ServiceAttachedQuestion: View {
    let store: StoreOf<QuestionFeature>
    private var question: DomainQuestion { store.question }
    var showsDetail = false

    @FocusState private var replyFocused: Bool
    private var picked: DomainV2.QuestionOption? { store.selectedOption }
    private var sending: Bool { store.isSubmitting || store.pendingSubmission != nil || store.needsReconciliation || store.completed }
    private var text: String { store.draft.text.trimmingCharacters(in: .whitespacesAndNewlines) }
    private var attachmentsEmpty: Bool { store.attachmentPicker.entries.isEmpty }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("The node asks", systemImage: "questionmark.bubble.fill")
                .font(.caption.weight(.bold))
                .textCase(.uppercase)
                .foregroundStyle(Palette.dawn)
            Text(question.title)
                .font(.headline)
                .foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
            if showsDetail, case let detail = question.body, !detail.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(MarkdownParser.blocks(from: detail)) { block in
                        MarkdownBlockView(block: block)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                .foregroundStyle(Palette.ink.opacity(0.9))
            }
            if !question.options.isEmpty {
                ServiceOptionGrid(options: question.options, picked: picked) {
                    replyFocused = false
                    store.send(.optionTapped($0.id))
                }
            }
            if showsDetail || question.options.isEmpty || picked != nil {
                replyField
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.dawn.opacity(0.1), in: .rect(cornerRadius: 20, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous).strokeBorder(Palette.dawn.opacity(0.35)))
        .disabled(sending)
        .opacity(sending ? 0.5 : 1)
        .animation(.spring(duration: 0.3), value: picked)
        .onDisappear { store.send(.disappeared) }
        .onChange(of: store.replyFocusRequest) { _, _ in replyFocused = true }
        .onChange(of: sending) { _, sending in if sending { replyFocused = false } }

    }

    private var replyField: some View {
        VStack(alignment: .leading, spacing: 8) {
            ServiceAttachmentStrip(store: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
            replyBox
        }
    }

    private var replyBox: some View {
        HStack(alignment: .bottom, spacing: 8) {
            ServiceAttachButton(store: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
                .padding(.leading, 4)
                .padding(.bottom, 2)
            TextField(String(localized: "Answer in your own words…"), text: Binding(get: { store.draft.text }, set: { store.send(.textChanged($0)) }), axis: .vertical)
                .lineLimit(1...4)
                .focused($replyFocused)
                .padding(.trailing, 14)
                .padding(.vertical, 10)
            Button {
                // A fast failure can complete before SwiftUI observes `sending`.
                replyFocused = false
                store.send(.answerTapped)
            } label: {
                Image(systemName: "arrow.up")
                    .font(.body.weight(.bold))
                    .foregroundStyle(Palette.onAccent)
                    .frame(width: 32, height: 32)
                    .background(text.isEmpty && attachmentsEmpty ? Palette.slate.opacity(0.4) : Palette.dawn, in: .circle)
            }
            .buttonStyle(PressStyle())
            .disabled(text.isEmpty && attachmentsEmpty)
            .padding(4)
            .accessibilityLabel("Send")
        }
        .background(Palette.cardRaised, in: .rect(cornerRadius: 20, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous).strokeBorder(replyFocused ? Palette.dawn.opacity(0.5) : Palette.hairline))
    }

}
