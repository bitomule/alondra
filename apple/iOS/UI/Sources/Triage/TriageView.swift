import AlondraFeatures
import AlondraDesign
import AlondraContract
import ComposableArchitecture
import SwiftUI

/// Waiting for You: a stack of cards in the order the server sends them. Swiping right answers with
/// a card's first button and swiping left with its last; a card whose button needs words or a
/// choice goes to the back of the stack instead. Tapping opens the card.
public struct TriageView: View {
    @Bindable var store: StoreOf<TriageFeature>
    let zoom: Namespace.ID
    @State private var drag: CGFloat = 0
    @GestureState private var dragging = false
    @State private var frontHeight: CGFloat?
    @State private var keyboardVisible = false
    @State private var screenWidth: CGFloat = 0
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.dynamicTypeSize) private var typeSize

    public init(store: StoreOf<TriageFeature>, zoom: Namespace.ID) {
        self.store = store
        self.zoom = zoom
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if !keyboardVisible {
                header.padding(.horizontal, 20)
                    .transition(.move(edge: .top).combined(with: .opacity))
            }
            if let error = store.loadError {
                ContentUnavailableView {
                    Label("Could not connect to the fleet", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(error)
                } actions: {
                    Button("Try Again") { store.send(.screen(.retry)) }
                        .buttonStyle(.borderedProminent).tint(Palette.dawn)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if store.screen.document == nil {
                VStack(spacing: 16) {
                    ProgressView().controlSize(.large).tint(Palette.dawn)
                    Text("Connecting to the fleet…").font(.subheadline).foregroundStyle(.secondary)
                }.frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if store.orderedIDs.isEmpty {
                empty
            } else {
                queue
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(DawnBackground())
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { screenWidth = $0 }
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button { store.send(.machinePresented(true)) } label: {
                    MachineBadge(gauges: gauges, width: screenWidth)
                }
                .popover(isPresented: Binding(get: { store.machinePresented }, set: { store.send(.machinePresented($0)) })) {
                    MachineDetail(gauges: gauges)
                        .presentationCompactAdaptation(.popover)
                }
                .accessibilityLabel("Mac Status")
            }
        }
        .task { await store.send(.task).finish() }
        .onDisappear { store.send(.disappeared) }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillShowNotification)) { _ in
            withAnimation(.smooth(duration: 0.3)) { keyboardVisible = true }
        }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillHideNotification)) { _ in
            withAnimation(.smooth(duration: 0.3)) { keyboardVisible = false }
        }
    }

    private var gauges: [AlondraContract.Gauge] {
        store.status?.gauges ?? store.screen.document?.status.gauges ?? []
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(store.queueTitle ?? String(localized: "Waiting for You"))
                .font(.display(.largeTitle, weight: .bold)).foregroundStyle(Palette.ink)
            if store.isOffline {
                Label("saved data · connection lost", systemImage: "wifi.slash")
                    .font(.subheadline.weight(.medium)).foregroundStyle(Palette.rose).lineLimit(1)
            } else {
                Text(store.status?.summary ?? store.screen.document?.status.summary ?? String(localized: "Connecting to the fleet…"))
                    .font(.subheadline.weight(.medium)).foregroundStyle(.secondary).lineLimit(1)
                    .contentTransition(.numericText())
            }
            if store.orderedIDs.count > 1 {
                pageDots.padding(.top, 6)
            }
        }
        .padding(.bottom, 14)
    }

    private var pageDots: some View {
        Group {
            if store.orderedIDs.count <= 8 {
                HStack(spacing: 6) {
                    ForEach(store.orderedIDs, id: \.self) { id in
                        let selected = id == store.currentID
                        Capsule().fill(selected ? Palette.dawn : Palette.slate.opacity(0.35))
                            .frame(width: selected ? 18 : 6, height: 6)
                    }
                }
            } else {
                Text("\(store.index + 1) of \(store.orderedIDs.count)")
                    .font(.caption.weight(.semibold).monospacedDigit())
                    .foregroundStyle(.secondary).contentTransition(.numericText())
            }
        }
        .animation(.spring(duration: 0.35, bounce: 0.2), value: store.index)
        .animation(.spring(duration: 0.4), value: store.orderedIDs.count)
        .accessibilityLabel("\(store.index + 1) of \(store.orderedIDs.count) pending")
    }

    /// The cards never scroll inside; at accessibility text sizes the whole stack does, so a card
    /// taller than the screen can still be read to its last button.
    @ViewBuilder private var queue: some View {
        if typeSize.isAccessibilitySize {
            ScrollView { stack.padding(.horizontal, 14).padding(.bottom, 8) }
                .scrollIndicators(.hidden)
                .scrollDismissesKeyboard(.interactively)
        } else {
            stack.padding(.horizontal, 14).padding(.bottom, 8)
        }
    }

    private var stack: some View {
        let ids = store.visibleIDs
        let browsable = store.orderedIDs.count > 1
        return ZStack(alignment: .top) {
            ForEach(ids.reversed(), id: \.self) { id in
                let depth = ids.firstIndex(of: id) ?? 0
                let dragged = id == store.swipe?.cardID
                Group {
                    if let cardStore = store.scope(state: \.cards[id: id], action: \.cards[id: id]) {
                        if let card = cardStore.card {
                            TriageCard(
                                store: cardStore, card: card, isFront: depth == 0, zoom: zoom,
                                onSkip: browsable ? { store.send(.leaveForLater) } : nil
                            )
                        } else {
                            CardPlaceholder(store: cardStore)
                        }
                    } else {
                        ProgressView().frame(maxWidth: .infinity, minHeight: 300)
                            .background(Palette.card, in: .rect(cornerRadius: 34, style: .continuous))
                    }
                }
                .id(id)
                .frame(maxHeight: depth == 0 ? nil : frontHeight, alignment: .top)
                .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height in
                    if depth == 0 { frontHeight = height }
                }
                .scaleEffect(1 - CGFloat(depth) * 0.05, anchor: .top)
                .offset(y: CGFloat(depth) * -11)
                .brightness(depth == 0 ? 0 : -0.04 * Double(depth))
                .offset(x: dragged ? drag : 0)
                .opacity(dragged ? 1 - Double(min(abs(drag) / 900, 0.35)) : 1)
                .zIndex(dragged ? 10 : Double(TriageFeature.depth - depth))
                .allowsHitTesting(depth == 0)
                .accessibilityHidden(depth != 0)
                .transition(reduceMotion ? .opacity : .asymmetric(
                    insertion: .opacity.combined(with: .scale(scale: 0.96, anchor: .top)), removal: .opacity
                ))
            }
        }
        .padding(.top, CGFloat(max(0, ids.count - 1)) * 11)
        .simultaneousGesture(swipe)
        .onChange(of: dragging) { _, now in
            guard !now else { return }
            Task { @MainActor in
                await Task.yield()
                guard store.swipe?.released == false else { return }
                store.send(.dragEnded(translation: 0, predicted: 0))
                withAnimation(.spring(duration: 0.4, bounce: 0.35)) { drag = 0 }
            }
        }
        .onChange(of: store.swipe?.leaving) { _, leaving in
            guard let leaving else { return }
            withAnimation(.easeIn(duration: 0.16)) { drag = leaving == .right ? 420 : -420 }
        }
        .onChange(of: store.swipe == nil) { _, settled in
            guard settled else { return }
            var instant = Transaction()
            instant.disablesAnimations = true
            withTransaction(instant) { drag = 0 }
        }
        .sensoryFeedback(.impact(weight: .light), trigger: store.skips)
        .animation(.spring(duration: 0.5, bounce: 0.22), value: ids)
    }

    private var swipe: some Gesture {
        DragGesture(minimumDistance: 24)
            .updating($dragging) { _, dragging, _ in dragging = true }
            .onChanged { value in
                if store.swipe == nil { store.send(.dragChanged) }
                guard store.swipe?.released == false,
                      abs(value.translation.width) > abs(value.translation.height) * 1.4 else { return }
                drag = value.translation.width
            }
            .onEnded { value in
                guard store.swipe?.released == false else { return }
                store.send(.dragEnded(translation: drag, predicted: value.predictedEndTranslation.width))
                if store.swipe?.leaving == nil {
                    withAnimation(.spring(duration: 0.4, bounce: 0.35)) { drag = 0 }
                }
            }
    }

    private var empty: some View {
        VStack(spacing: 18) {
            Spacer()
            Image(systemName: "sunrise.fill")
                .font(.system(size: 64, weight: .light))
                .foregroundStyle(LinearGradient(colors: [Palette.dawn, Palette.ember], startPoint: .top, endPoint: .bottom))
                .symbolEffect(.breathe.pulse, options: .repeat(.continuous))
            Text(store.screen.document?.empty?.text ?? String(localized: "Nothing is waiting for you"))
                .font(.display(.title, weight: .bold))
            Text("The fleet carries on by itself. When a node needs you, it will show up here.")
                .font(.body).foregroundStyle(.secondary).multilineTextAlignment(.center).frame(maxWidth: 300)
            HStack(spacing: 10) {
                emptyStat("working", count: statusCount("working"), tint: Palette.sky)
                emptyStat("waiting", count: statusCount("waiting"), tint: Palette.dawn)
                emptyStat("total", count: statusCount("agents"), tint: Palette.sage)
            }.padding(.top, 8)
            Spacer(); Spacer()
        }
        .padding(.horizontal, 24).frame(maxWidth: .infinity)
    }

    private func statusCount(_ id: String) -> Int {
        Int((store.status ?? store.screen.document?.status)?.counts.first(where: { $0.id == id })?.value ?? 0)
    }

    private func emptyStat(_ label: LocalizedStringKey, count: Int, tint: Color) -> some View {
        VStack(spacing: 2) {
            Text(count, format: .number).font(.display(.title2, weight: .bold))
                .foregroundStyle(tint).contentTransition(.numericText())
            Text(label).font(.caption).foregroundStyle(.secondary)
        }
        .frame(minWidth: 88).padding(.vertical, 12)
        .background(Palette.card.opacity(0.7), in: .rect(cornerRadius: 18))
    }
}

private struct CardPlaceholder: View {
    let store: StoreOf<TriageCardFeature>

    var body: some View {
        Group {
            switch store.screen.phase {
            case .failed(let message):
                ContentUnavailableView {
                    Label("Could not connect to the fleet", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(message)
                } actions: {
                    Button("Try Again") { store.send(.screen(.retry)) }.buttonStyle(.glass)
                }
            case .missing, .loaded:
                ContentUnavailableView("This screen is no longer available", systemImage: "tray")
            case .loading:
                ProgressView()
            }
        }
        .frame(maxWidth: .infinity, minHeight: 300)
        .background(Palette.card, in: .rect(cornerRadius: 34, style: .continuous))
    }
}
