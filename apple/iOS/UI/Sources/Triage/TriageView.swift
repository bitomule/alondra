import AlondraFeatures
import AlondraDesign
import AlondraContract
import ComposableArchitecture
import SwiftUI

/// Waiting for You: a stack of cards in the order the server sends them. Swiping either way sends
/// the front card to the back and answers nothing; only a tap on one of its buttons answers it.
/// Tapping elsewhere on it opens the card.
public struct TriageView: View {
    @Bindable var store: StoreOf<TriageFeature>
    let zoom: Namespace.ID
    @State private var drag: CGFloat = 0
    @GestureState private var dragging = false
    @State private var keyboardVisible = false
    @State private var screenWidth: CGFloat = 0
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.dynamicTypeSize) private var typeSize
    @Environment(\.connectionState) private var connection

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
            if let setAside = store.setAside, let cards = store.setAsideCards {
                SetAsideAnswersBanner(cards: cards, othersMayBeMissing: setAside.othersMayBeMissing) {
                    store.send(.setAsideDismissed, animation: .smooth)
                }
                .padding(.horizontal, 16).padding(.bottom, 10)
                .transition(.move(edge: .top).combined(with: .opacity))
            }
            if let usage = store.status?.usageExhausted {
                UsageExhaustedBanner(usage)
                    .padding(.horizontal, 16).padding(.bottom, 10)
                    .transition(.move(edge: .top).combined(with: .opacity))
            } else if let wait = store.status?.sessionWait {
                SessionWaitBanner(wait)
                    .padding(.horizontal, 16).padding(.bottom, 10)
                    .transition(.move(edge: .top).combined(with: .opacity))
            }
            if let error = store.loadError, indicators.cover == .none {
                ContentUnavailableView {
                    Label("Couldn’t load this", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(error)
                } actions: {
                    Button("Try Again") { store.send(.screen(.retry)) }
                        .buttonStyle(.borderedProminent).tint(Palette.dawn)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if store.screen.document == nil {
                centeredScrolling {
                    VStack(spacing: 16) {
                        ProgressView().controlSize(.large).tint(Palette.dawn)
                        Text("Connecting to your Mac…").font(.subheadline).foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                    }
                    .padding(.horizontal, 24).frame(maxWidth: .infinity)
                }
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
                    MachineBadge(gauges: gauges, light: indicators.light, width: screenWidth)
                }
                .popover(isPresented: Binding(get: { store.machinePresented }, set: { store.send(.machinePresented($0)) })) {
                    MachineDetail(gauges: gauges)
                        .presentationCompactAdaptation(.popover)
                }
                .accessibilityLabel("Mac Status")
                .accessibilityIdentifier("mac.status.\(String(describing: indicators.light))")
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
        store.status?.gauges ?? []
    }

    private var indicators: ConnectionIndicators {
        ConnectionIndicators(connection, hasSavedData: store.screen.document != nil)
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(store.queueTitle ?? String(localized: "Waiting for You"))
                .font(.display(.largeTitle, weight: .bold)).foregroundStyle(Palette.ink)
                .lineLimit(typeSize.isAccessibilitySize ? 1 : nil)
                .minimumScaleFactor(0.5)
            ConnectionLine(indicators.line, alert: Palette.rose) {
                summary.foregroundStyle(.secondary).contentTransition(.numericText())
            }
            .font(.subheadline.weight(.medium)).lineLimit(typeSize.isAccessibilitySize ? nil : 1)
            HStack(alignment: .center) {
                pageDots
                    .opacity(store.orderedIDs.count > 1 ? 1 : 0)
                    .accessibilityHidden(store.orderedIDs.count <= 1)
                Spacer(minLength: 8)
                if !store.arrivals.isEmpty {
                    arrivalsButton.transition(.scale(scale: 0.8).combined(with: .opacity))
                }
            }
            .padding(.top, 6)
            .animation(.spring(duration: 0.35, bounce: 0.2), value: store.arrivals.count)
        }
        .padding(.bottom, 14)
    }

    private var summary: Text {
        guard let header = store.header else {
            return Text(store.status?.summary ?? String(localized: "Connecting to your Mac…"))
        }
        var parts: [Text] = []
        if header.questions > 0 { parts.append(Text("\(header.questions) questions")) }
        if header.toRead > 0 { parts.append(Text("\(header.toRead) to read")) }
        parts.append(Text("\(header.working) nodes working"))
        return parts.dropFirst().reduce(parts[0]) { line, part in Text("\(line) · \(part)") }
    }

    /// Cards that arrived while the person was reading wait right behind; this brings the first forward.
    private var arrivalsButton: some View {
        Button { store.send(.showArrivals, animation: .spring(duration: 0.5, bounce: 0.22)) } label: {
            Label("\(store.arrivals.count) new cards", systemImage: "arrow.up")
                .font(.caption.weight(.semibold))
                .contentTransition(.numericText())
                .padding(.horizontal, 10).padding(.vertical, 5)
                .background(Palette.dawn.opacity(0.16), in: .capsule)
                .foregroundStyle(Palette.dawn)
        }
        .buttonStyle(.plain)
        .accessibilityHint("Shows the first card that arrived while you were reading")
        .accessibilityIdentifier("triage.arrivals")
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

    /// Every card is the room the stack has, from under the header down to the bottom margin above
    /// the tab bar, so the ones behind line up with the front one. The room for the page dots and
    /// for the cards peeking behind is kept even when there are none, so a card alone is as tall as
    /// one in a full stack. The card never grows with its content and nothing here scrolls: the
    /// content adapts to the card, and what does not fit is in the detail.
    private var queue: some View {
        GeometryReader { proxy in
            stack(height: proxy.size.height - Self.bottomMargin)
                .padding(.horizontal, 14).padding(.bottom, Self.bottomMargin)
        }
    }

    private static let bottomMargin: CGFloat = 36
    private static let peek = CGFloat(TriageFeature.depth - 1) * 11

    private func stack(height: CGFloat) -> some View {
        let ids = store.visibleIDs
        let cardHeight = max(0, height - Self.peek)
        let browsable = store.orderedIDs.count > 1
        let drawnInFull = store.swipe == nil ? 1 : 2
        return ZStack(alignment: .top) {
            ForEach(ids.reversed(), id: \.self) { id in
                let depth = ids.firstIndex(of: id) ?? 0
                let dragged = id == store.swipe?.cardID
                Group {
                    if depth >= drawnInFull {
                        StackedCardShape(tint: store.cards[id: id]?.card.map { Kind($0).tint })
                    } else if let cardStore = store.scope(state: \.cards[id: id], action: \.cards[id: id]) {
                        if let card = cardStore.card {
                            TriageCard(
                                store: cardStore, card: card, isFront: depth == 0, zoom: zoom,
                                onSkip: browsable ? { store.send(.leaveForLater) } : nil
                            )
                        } else {
                            CardPlaceholder(store: cardStore)
                        }
                    } else {
                        CardSkeleton()
                    }
                }
                .id(id)
                .frame(height: cardHeight, alignment: .top)
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
        .padding(.top, Self.peek)
        // Above the card's buttons, not beside them: a swipe that starts on a button would also
        // press it when the finger lifts, and answer the card the person only meant to move.
        .highPriorityGesture(swipe)
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

    /// Centred when it fits; when text is large enough that it does not, the person scrolls to read it all.
    private func centeredScrolling<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        let body = content()
        return GeometryReader { proxy in
            ScrollView {
                body.frame(maxWidth: .infinity, minHeight: proxy.size.height)
            }
            .scrollBounceBehavior(.basedOnSize)
        }
    }

    private var empty: some View {
        centeredScrolling {
            VStack(spacing: 18) {
                Image(systemName: "sunrise.fill")
                    .font(.system(size: 64, weight: .light))
                    .foregroundStyle(LinearGradient(colors: [Palette.dawn, Palette.ember], startPoint: .top, endPoint: .bottom))
                    .symbolEffect(.breathe.pulse, options: .repeat(.continuous))
                Text(store.screen.document?.empty?.text ?? String(localized: "Nothing is waiting for you"))
                    .font(.display(.title, weight: .bold)).multilineTextAlignment(.center)
                Text("Your tasks carry on by themselves. When one needs you, it will show up here.")
                    .font(.body).foregroundStyle(.secondary).multilineTextAlignment(.center)
                    .frame(maxWidth: typeSize.isAccessibilitySize ? .infinity : 300)
                let stats = typeSize.isAccessibilitySize
                    ? AnyLayout(VStackLayout(spacing: 10)) : AnyLayout(HStackLayout(spacing: 10))
                stats {
                    emptyStat("working", count: statusCount("working"), tint: Palette.sky)
                    emptyStat("waiting", count: statusCount("waiting"), tint: Palette.dawn)
                    emptyStat("total", count: statusCount("agents"), tint: Palette.sage)
                }.padding(.top, 8)
            }
            .padding(.horizontal, 24).padding(.vertical, 24)
        }
    }

    private func statusCount(_ id: String) -> Int {
        Int(store.status?.counts.first(where: { $0.id == id })?.value ?? 0)
    }

    private func emptyStat(_ label: LocalizedStringKey, count: Int, tint: Color) -> some View {
        VStack(spacing: 2) {
            Text(count, format: .number).font(.display(.title2, weight: .bold))
                .foregroundStyle(tint).contentTransition(.numericText())
            Text(label).font(.caption).foregroundStyle(.secondary)
        }
        .frame(minWidth: 88, maxWidth: typeSize.isAccessibilitySize ? .infinity : nil).padding(.vertical, 12)
        .background(Palette.card.opacity(0.7), in: .rect(cornerRadius: 18))
    }
}

private struct CardPlaceholder: View {
    let store: StoreOf<TriageCardFeature>

    var body: some View {
        switch store.screen.phase {
        case .failed(let message):
            unavailable {
                ContentUnavailableView {
                    Label("Couldn’t load this card", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(message)
                } actions: {
                    Button("Try Again") { store.send(.screen(.retry)) }.buttonStyle(.glass)
                }
            }
        case .missing, .loaded:
            unavailable { ContentUnavailableView("This screen is no longer available", systemImage: "tray") }
        case .loading:
            CardSkeleton()
        }
    }

    private func unavailable<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        content()
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Palette.card, in: .rect(cornerRadius: 34, style: .continuous))
            .overlay { RoundedRectangle(cornerRadius: 34, style: .continuous).strokeBorder(Palette.hairline) }
    }
}
