import ComposableArchitecture
import SwiftUI

/// What a card shows while its answer waits on a Mac that does not answer: that it keeps trying,
/// and a button to try again now.
public struct StallNoticeView: View {
    let store: StoreOf<ActionFeature>

    public init(store: StoreOf<ActionFeature>) { self.store = store }

    public var body: some View {
        if let notice = store.stallNotice {
            HStack(spacing: 8) {
                Label(notice, systemImage: "arrow.triangle.2.circlepath")
                    .font(.caption)
                    .foregroundStyle(.orange)
                Spacer(minLength: 0)
                Button("Retry now") { store.send(.retryNow) }
                    .font(.caption.weight(.semibold))
                    .buttonStyle(.borderless)
            }
            .accessibilityElement(children: .combine)
        }
    }
}
