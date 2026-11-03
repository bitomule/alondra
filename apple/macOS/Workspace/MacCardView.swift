import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// One card as the Mac shows it, drawn from what the server sends: the kind, what is needed, why,
/// one line of content, what it holds back, the buttons in a column and a reply field. Everything
/// longer is in the detail below it.
struct MacCardView: View {
    let card: Card
    var excludedActionIDs: Set<String> = []
    /// Why the card is back: the last answer to it did not go through.
    var returnReason: String?
    var onAction: (AlondraContract.Action) -> Void
    var onReply: (AlondraContract.Action, String) -> Void
    var onLink: (AlondraContract.Link) -> Void
    var onContent: () -> Void
    var onFilledForm: (AlondraContract.Action, RequestedForm, [String: JSONValue]) -> Void = { _, _, _ in }

    @State private var reply = ""
    @State private var moreOpen = false
    /// What the person changed in the card's form; until then the form shows its defaults.
    @State private var formValues: [String: JSONValue]?
    @State private var formProblem: String?
    @FocusState private var replyFocused: Bool

    private var buttons: MacCardButtons { MacCardButtons(card, excluding: excludedActionIDs) }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            MacCardHeader(card: card)
            VStack(alignment: .leading, spacing: 6) {
                Text(MarkdownBlock.line(card.need ?? card.title)).font(.title3.weight(.semibold)).fixedSize(horizontal: false, vertical: true)
                if let why = card.why {
                    if card.detail == .included {
                        MarkdownView(markdown: Markdown(text: why), secondary: true).fixedSize(horizontal: false, vertical: true)
                    } else {
                        Text(MarkdownBlock.line(why)).font(.body).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    }
                }
                if let meta = card.meta { Text(meta).font(.caption).foregroundStyle(.tertiary) }
            }
            if let row = card.contentRow { MacContentRow(row: row, open: onContent) }
            if !card.blocks.isEmpty || !card.blockedBy.isEmpty { MacBlockChips(card: card, open: onLink) }
            if let form = card.form {
                RequestedFormView(form: form, values: Binding(get: { formValues ?? form.initialValues }, set: { formValues = $0; formProblem = nil }))
                    .onChange(of: form) { formValues = nil; formProblem = nil }
                if let formProblem {
                    Text(formProblem).font(.callout).foregroundStyle(MacCardPalette.risk).fixedSize(horizontal: false, vertical: true)
                }
            }
            MacActionColumn(buttons: buttons, moreOpen: $moreOpen, onAction: press)
            if let returnReason {
                Text(returnReason).font(.callout).foregroundStyle(MacCardPalette.risk).fixedSize(horizontal: false, vertical: true)
            }
            if let action = card.reply {
                MacReplyField(action: action, tint: MacCardPalette.kind(card.kind), text: $reply, focus: $replyFocused) {
                    send(action)
                }
            }
        }
        .padding(18)
        .frame(maxWidth: 640, alignment: .leading)
        .background(MacCardPalette.surface, in: .rect(cornerRadius: 18, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous).strokeBorder(MacCardPalette.hairline))
        .background {
            if card.detail == .included {
                MacShortcuts(buttons: buttons, moreOpen: $moreOpen, onAction: press, reply: card.reply != nil ? { replyFocused = true } : nil)
                    .disabled(replyFocused)
            }
        }
        .accessibilityElement(children: .contain)
    }

    /// An option answers at once; what the person already wrote in the card's field goes with it.
    private func press(_ action: AlondraContract.Action) {
        if let form = card.form, form.isFilled(by: action) {
            let values = formValues ?? form.initialValues
            formProblem = form.problem(values)
            if formProblem == nil { onFilledForm(action, form, values) }
            return
        }
        guard let words = action.carrying(reply) else { return onAction(action) }
        reply = ""
        onReply(action, words)
    }

    private func send(_ action: AlondraContract.Action) {
        let text = reply.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty || action.comments != nil else { return }
        reply = ""
        onReply(action, text)
    }
}
