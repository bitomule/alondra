import AlondraDesign
import AlondraFeed
import SwiftUI

/// What the banner can say about the cards whose answers were set aside.
public enum SetAsideCards: Equatable, Sendable {
    /// Every answer set aside resolved no card.
    case none
    /// Part of the store could not be read, so which cards is not known.
    case unknown
    case noneWaiting
    case waiting(Int)
}

public extension SetAsideAnswers {
    /// `queue` is the queue's card IDs; nil while it has not loaded.
    func cardsLine(queue: [String]?) -> SetAsideCards {
        guard complete else { return .unknown }
        guard !cards.isEmpty else { return .none }
        guard let queue else { return .unknown }
        let waiting = cards.filter(queue.contains).count
        return waiting == 0 ? .noneWaiting : .waiting(waiting)
    }

    var othersMayBeMissing: Bool { !complete || otherActions > 0 }
}

/// Stays until the person dismisses it: answers this device set aside are never sent again, so
/// it is the only way they learn what to answer once more.
public struct SetAsideAnswersBanner: View {
    let cards: SetAsideCards
    let othersMayBeMissing: Bool
    let dismiss: () -> Void

    public init(cards: SetAsideCards, othersMayBeMissing: Bool, dismiss: @escaping () -> Void) {
        self.cards = cards; self.othersMayBeMissing = othersMayBeMissing; self.dismiss = dismiss
    }

    public var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: "exclamationmark.arrow.triangle.2.circlepath")
                .foregroundStyle(Palette.rose)
            VStack(alignment: .leading, spacing: 4) {
                Text("Your Mac didn’t confirm some answers")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Palette.ink)
                Group {
                    Text("Alondra couldn’t read the answers this device was still sending, so it set them aside instead of sending them again.")
                    switch cards {
                    case .none: EmptyView()
                    case .unknown: Text("Any card you answered that’s waiting again needs a new answer.")
                    case .noneWaiting: Text("None of those cards is waiting for you anymore.")
                    case .waiting(1): Text("One of those cards is waiting for you again: answer it once more.")
                    case .waiting(let count): Text("\(count) of those cards are waiting for you again: answer them once more.")
                    }
                    if othersMayBeMissing {
                        Text("Nor did it confirm what else you sent from this device just before; check it on your Mac.")
                    }
                }
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
            Button("OK", action: dismiss)
                .font(.footnote.weight(.semibold))
                .buttonStyle(.borderless)
                .tint(Palette.dawn)
                .accessibilityHint("Hides this notice")
        }
        .padding(.horizontal, 14).padding(.vertical, 12)
        .modifier(BannerBackground())
        .accessibilityElement(children: .contain)
    }
}

/// Glass on iOS, over the queue's dawn background; a tinted plate on the Mac, like its other notices.
private struct BannerBackground: ViewModifier {
    func body(content: Content) -> some View {
        #if os(macOS)
        content.background(Palette.rose.opacity(0.10), in: .rect(cornerRadius: 12))
        #else
        content.glassEffect(.regular.tint(Palette.rose.opacity(0.12)), in: .rect(cornerRadius: 18))
        #endif
    }
}
