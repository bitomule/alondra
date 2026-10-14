import AlondraAttachments
import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// A card's content as one step of `CardFit` lays it out. The buttons and the room for the reply
/// field keep their place at the bottom; what the fit gives up lives in the detail.
struct CardBody: View {
    let store: StoreOf<TriageCardFeature>
    let card: Card
    let fit: CardFit
    let tint: Color
    let onSkip: (() -> Void)?
    @Binding var room: ComposerRoom?
    let roomFocus: FocusState<Bool>.Binding
    @Environment(\.openURL) private var openURL
    @Environment(\.dynamicTypeSize) private var typeSize

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            CardHeader(card: card, showsState: fit.state, onSkip: onSkip)
            CardSummary(card: card, why: fit.why, meta: fit.meta, large: typeSize.isAccessibilitySize) { store.openDetail(card) }
            if fit.row, let row = card.contentRow {
                CardContentRow(row: row, tint: tint) { store.openDetail(card) }
            }
            if fit.chips, !card.badges.isEmpty {
                HStack(spacing: 10) { ForEach(card.badges, id: \.text) { ToneText(label: $0) } }
            }
            if fit.chips {
                BlockChips(blocks: card.blocks, blockedBy: card.blockedBy) { store.open($0, openURL: openURL) }
            }
            Spacer(minLength: 0)
            if let error = store.actions.error { Text(error).font(.caption).foregroundStyle(Palette.rose) }
            CardButtons(store: store, cardID: card.id)
            if fit.reply, let reply = store.replyAction {
                AttachmentStrip(store: store.scope(state: \.attachments, action: \.attachments))
                ComposerRoom.Placeholder(fit: fit, room: $room) {
                    CardComposer(store: store, action: reply, cardID: card.id, tint: tint, focus: roomFocus)
                }
            }
        }
        .dynamicTypeSize(...fit.largestType)
    }
}
