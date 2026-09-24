import AlondraShared
import SwiftUI

/// A question the node asks about the PR it sits in, answered right there without leaving the PR.
struct AttachedQuestion: View {
    let question: Question
    var showsDetail = false

    @Environment(FleetStore.self) private var fleet
    @State private var reply = ""
    @State private var picked: QuestionOption?
    @State private var sending = false
    @FocusState private var replyFocused: Bool

    private var text: String { reply.trimmingCharacters(in: .whitespacesAndNewlines) }

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
            if showsDetail, let detail = question.readableDetail, !detail.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(MarkdownParser.blocks(from: detail)) { block in
                        MarkdownBlockView(block: block)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                .foregroundStyle(Palette.ink.opacity(0.9))
            }
            if !question.options.isEmpty {
                OptionGrid(options: question.options, picked: picked, onPick: choose)
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
    }

    private var replyField: some View {
        HStack(alignment: .bottom, spacing: 8) {
            TextField(picked?.displayDescription ?? String(localized: "Answer in your own words…"), text: $reply, axis: .vertical)
                .lineLimit(1...4)
                .focused($replyFocused)
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
            Button(action: submit) {
                Image(systemName: "arrow.up")
                    .font(.body.weight(.bold))
                    .foregroundStyle(Palette.onAccent)
                    .frame(width: 32, height: 32)
                    .background(text.isEmpty ? Palette.slate.opacity(0.4) : Palette.dawn, in: .circle)
            }
            .buttonStyle(PressStyle())
            .disabled(text.isEmpty)
            .padding(4)
            .accessibilityLabel("Send")
        }
        .background(Palette.cardRaised, in: .rect(cornerRadius: 20, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous).strokeBorder(replyFocused ? Palette.dawn.opacity(0.5) : Palette.hairline))
    }

    private func choose(_ option: QuestionOption) {
        if TriageCard.needsText(option), text.isEmpty {
            picked = option
            replyFocused = true
            return
        }
        answer(text.isEmpty ? option.label : text, option: option.label)
    }

    private func submit() {
        guard !text.isEmpty else { return }
        answer(text, option: picked?.label)
    }

    private func answer(_ text: String, option: String?) {
        let id = question.id
        replyFocused = false
        sending = true
        Task {
            let answered = await fleet.resolve(.question(question)) { try await $0.answer(id, text: text, option: option) }
            if !answered { sending = false }
        }
    }
}
