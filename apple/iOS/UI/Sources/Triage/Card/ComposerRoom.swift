import SwiftUI

/// Where the picked layout keeps room for the reply field, and the text size it has there. Each
/// layout only keeps the room; the field itself is drawn once, over it. A field inside the layouts
/// would be rebuilt, and lose the keyboard, whenever the keyboard shrinks the card and another
/// layout is picked.
struct ComposerRoom: Equatable {
    static let space = "composer-room"
    let fit: CardFit
    let frame: CGRect
    let typeSize: DynamicTypeSize

    struct Placeholder<Content: View>: View {
        let fit: CardFit
        @Binding var room: ComposerRoom?
        @ViewBuilder let content: Content
        @Environment(\.dynamicTypeSize) private var typeSize

        var body: some View {
            content.hidden()
                .onGeometryChange(for: CGRect.self) { $0.frame(in: .named(space)) } action: { frame in
                    room = ComposerRoom(fit: fit, frame: frame, typeSize: typeSize)
                }
                .onDisappear { if room?.fit == fit { room = nil } }
        }
    }
}
