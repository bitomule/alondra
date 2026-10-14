import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// One card of Waiting for You, drawn from what the server sends: the 34 pt shell, its content as
/// the first `CardFit` that fits, the reply field over the room that fit keeps for it, and the
/// stamp that covers it the moment an answer is sent. The card is always as tall as the stack
/// makes it and nothing inside scrolls: the content adapts to the card.
struct TriageCard: View {
    @Bindable var store: StoreOf<TriageCardFeature>
    let card: Card
    let isFront: Bool
    let zoom: Namespace.ID
    let onSkip: (() -> Void)?
    @FocusState private var replyFocused: Bool
    @FocusState private var roomFocused: Bool
    @State private var room: ComposerRoom?
    @Environment(\.dynamicTypeSize) private var typeSize

    private var tint: Color { Kind(card).tint }
    private var stamp: String? { store.stamp }

    var body: some View {
        shell
            .actionDialogs(store.scope(state: \.actions, action: \.actions))
            .sheet(item: Binding(get: { store.composition }, set: { if $0 == nil { store.send(.compositionCancelled) } })) { composition in
                CompositionSheet(
                    composition: composition, tint: tint,
                    reasonChanged: { store.send(.reasonChanged($0)) }, noteChanged: { store.send(.noteChanged($0)) },
                    cancel: { store.send(.compositionCancelled) }, send: { store.send(.compositionConfirmed) }
                )
            }
    }

    private var shell: some View {
        Color.clear
            .overlay(alignment: .topLeading) {
                content.padding(.horizontal, 20).padding(.vertical, 20)
                    .allowsHitTesting(stamp == nil)
                    .accessibilityHidden(stamp != nil)
            }
            .blur(radius: stamp == nil ? 0 : 10)
            .opacity(stamp == nil ? 1 : 0.4)
            .background { CardSurface(tint: tint) }
            .clipShape(.rect(cornerRadius: 34, style: .continuous))
            .matchedTransitionSource(id: card.id, in: zoom)
            .overlay { RoundedRectangle(cornerRadius: 34, style: .continuous).strokeBorder(Palette.hairline) }
            .overlay {
                if let stamp { CardStamp(stamp: stamp, tint: tint).transition(.opacity) }
            }
            .shadow(color: .black.opacity(isFront ? 0.22 : 0.08), radius: isFront ? 24 : 8, y: isFront ? 14 : 4)
            .scaleEffect(stamp == nil ? 1 : 1.02)
            .animation(.spring(duration: 0.35, bounce: 0.4), value: stamp)
            .accessibilityAction(named: "Open") { store.openDetail(card) }
            .accessibilityActions {
                if let onSkip { Button("Leave for Later", action: onSkip) }
            }
    }

    private var content: some View {
        ViewThatFits(in: .vertical) {
            ForEach(CardFit.ladder) { fit in
                CardBody(store: store, card: card, fit: fit, tint: tint, onSkip: onSkip, room: $room, roomFocus: $roomFocused)
            }
        }
        .coordinateSpace(.named(ComposerRoom.space))
        .overlay(alignment: room == nil ? .bottomLeading : .topLeading) {
            if let reply = store.replyAction { placedComposer(reply) }
        }
        .disabled(store.actions.inFlight && store.actions.stamp == nil && !store.actions.stalled)
    }

    /// With no room in the picked layout the field stays over the bottom of the card while it has
    /// the keyboard, so the person keeps writing; once they leave it, the buttons show again.
    private func placedComposer(_ reply: AlondraContract.Action) -> some View {
        let shown = (room != nil || replyFocused) && stamp == nil
        return CardComposer(store: store, action: reply, cardID: card.id, tint: tint, focus: $replyFocused)
            .dynamicTypeSize(room?.typeSize ?? typeSize)
            .frame(width: room?.frame.width)
            .offset(x: room?.frame.minX ?? 0, y: room?.frame.minY ?? 0)
            .opacity(shown ? 1 : 0)
            .allowsHitTesting(shown)
            .accessibilityHidden(!shown)
    }
}
