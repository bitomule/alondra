import AlondraShared
import AlondraTriageFeature
import ComposableArchitecture
import SwiftUI

/// iOS supplies its existing TriageCard as `card`, preserving its visual composition while F1
/// owns the queue, cursor and navigation decision. Local drag is presentation state only.
#if os(iOS)
public struct TriageDeck<Card: View>: View {
    let store: StoreOf<TriageFeature>
    let card: (InboxItem, Bool, Namespace.ID, @escaping () -> Void, @escaping () -> Void, @escaping () -> Void) -> Card
    @Namespace private var zoom
    @State private var drag: CGFloat = 0
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    public init(
        store: StoreOf<TriageFeature>,
        @ViewBuilder card: @escaping (InboxItem, Bool, Namespace.ID, @escaping () -> Void, @escaping () -> Void, @escaping () -> Void) -> Card
    ) {
        self.store = store
        self.card = card
    }

    public var body: some View {
        let visible = Array(store.queue.prefix(3))
        ZStack(alignment: .top) {
            if visible.isEmpty && !store.hasSnapshot {
                Label("Waiting for the server", systemImage: "wifi.slash")
                    .foregroundStyle(.secondary)
            }
            ForEach(visible.reversed()) { item in
                let depth = visible.firstIndex(where: { $0.id == item.id }) ?? 0
                card(item, depth == 0, zoom,
                     { store.send(.review(item.id)) },
                     { store.send(.skip(item.id)) },
                     { store.send(.hide(item.id)) })
                    .scaleEffect(1 - CGFloat(depth) * 0.05, anchor: .top)
                    .offset(y: CGFloat(depth) * -11)
                    .offset(x: depth == 0 ? drag : 0)
                    .zIndex(Double(3 - depth))
                    .allowsHitTesting(depth == 0)
                    .accessibilityHidden(depth != 0)
            }
        }
        .padding(.top, CGFloat(max(visible.count - 1, 0)) * 11)
        .simultaneousGesture(swipe(front: visible.first, count: store.queue.count))
        .animation(reduceMotion ? nil : .spring(duration: 0.5, bounce: 0.22), value: visible.map(\.id))
    }

    private func swipe(front: InboxItem?, count: Int) -> some Gesture {
        DragGesture(minimumDistance: 24)
            .onChanged { value in
                guard front != nil, count > 1,
                      abs(value.translation.width) > abs(value.translation.height) * 1.4 else { return }
                drag = value.translation.width
            }
            .onEnded { value in
                defer { withAnimation(.spring(duration: 0.4)) { drag = 0 } }
                guard let front, count > 1 else { return }
                let projected = value.predictedEndTranslation.width
                if abs(drag) > 90 || (abs(projected) > 240 && abs(drag) > 30) {
                    store.send(.skip(front.id))
                }
            }
    }
}
#endif

/// macOS supplies its existing InboxRow as `row`; this keeps row text and accessibility while
/// keeping selection and queue state in the TCA feature.
#if os(macOS)
public struct TriageSidebarRows<Row: View>: View {
    let store: StoreOf<TriageFeature>
    let row: (InboxItem) -> Row

    public init(store: StoreOf<TriageFeature>, @ViewBuilder row: @escaping (InboxItem) -> Row) {
        self.store = store
        self.row = row
    }

    public var body: some View {
        Section {
            if store.queue.isEmpty && !store.hasSnapshot {
                Label("Waiting for the server", systemImage: "wifi.slash")
                    .foregroundStyle(.secondary)
            } else if store.queue.isEmpty {
                Label("All caught up", systemImage: "checkmark.circle")
                    .foregroundStyle(.secondary)
            }
            ForEach(store.queue.filter { !$0.isPullRequest }) { item in
                Button { store.send(.review(item.id)) } label: { row(item) }
                    .buttonStyle(.plain)
            }
        } header: {
            Text("Waiting for You")
        }
        if store.queue.contains(where: \.isPullRequest) {
            Section("Pull Requests") {
                ForEach(store.queue.filter(\.isPullRequest)) { item in
                    Button { store.send(.review(item.id)) } label: { row(item) }
                        .buttonStyle(.plain)
                }
            }
        }
    }
}
#endif
