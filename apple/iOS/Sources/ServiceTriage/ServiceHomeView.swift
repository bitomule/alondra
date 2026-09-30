import AlondraHomeFeature
import AlondraShared
import AlondraViews
import AlondraViewsUI
import ComposableArchitecture
import SwiftUI

/// The iOS home retains the existing card stack geometry. Its content comes from the
/// server's ordered home view and its buttons use ActionFeature.
struct ServiceHomeView: View {
    @Bindable var store: StoreOf<HomeFeature>
    let zoom: Namespace.ID
    let openReview: (String) -> Void
    @State private var drag: CGFloat = 0
    @State private var frontHeight: CGFloat?
    @State private var keyboardVisible = false
    @State private var machinePresented = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if !keyboardVisible {
                header.padding(.horizontal, 20)
                    .transition(.move(edge: .top).combined(with: .opacity))
            }
            if store.document == nil, let error = store.error {
                ContentUnavailableView {
                    Label("Could not connect to the fleet", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(error)
                } actions: {
                    Button("Try Again") { store.send(.appeared) }
                        .buttonStyle(.borderedProminent).tint(Palette.dawn)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if store.document == nil {
                VStack(spacing: 16) {
                    ProgressView().controlSize(.large).tint(Palette.dawn)
                    Text("Connecting to the fleet…").font(.subheadline).foregroundStyle(.secondary)
                }.frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if store.orderedIDs.isEmpty {
                empty
            } else {
                stack.padding(.horizontal, 14).padding(.bottom, 8)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(DawnBackground())
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button { machinePresented = true } label: {
                    HomeMachineBadge(gauges: store.status?.gauges ?? store.document?.status.gauges ?? [])
                }
                .popover(isPresented: $machinePresented) {
                    HomeMachineDetail(gauges: store.status?.gauges ?? store.document?.status.gauges ?? [])
                        .presentationCompactAdaptation(.popover)
                }
                .accessibilityLabel("Mac Status")
            }
        }
        .task { await store.send(.appeared).finish() }
        .onDisappear { store.send(.disappeared) }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillShowNotification)) { _ in
            withAnimation(.smooth(duration: 0.3)) { keyboardVisible = true }
        }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillHideNotification)) { _ in
            withAnimation(.smooth(duration: 0.3)) { keyboardVisible = false }
        }
        .alert("Check Alondra", isPresented: Binding(get: { store.document != nil && store.error != nil }, set: { if !$0 { store.send(.dismissError) } })) {
            Button("OK") { store.send(.dismissError) }
        } message: { Text(store.error ?? "") }
        .alert(store.actions.confirmation?.title ?? "", isPresented: Binding(get: { store.actions.confirmation != nil }, set: { if !$0 { store.send(.actions(.cancelConfirmation)) } })) {
            Button(store.actions.confirmation?.button ?? String(localized: "Continue")) { store.send(.actions(.confirm)) }
            Button("Cancel", role: .cancel) { store.send(.actions(.cancelConfirmation)) }
        } message: { Text(store.actions.confirmation?.message ?? "") }
        .alert("This item changed", isPresented: Binding(get: { store.actions.changed != nil }, set: { if !$0 { store.send(.actions(.dismiss)) } })) {
            Button(store.actions.changed?.button ?? String(localized: "Continue")) { store.send(.actions(.sendChangedAction)) }
            Button("Cancel", role: .cancel) { store.send(.actions(.dismiss)) }
        } message: { Text(store.actions.error ?? "") }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(store.document?.sections.first(where: { $0.id == "queue" })?.title ?? String(localized: "Waiting for You"))
                .font(.display(.largeTitle, weight: .bold)).foregroundStyle(Palette.ink)
            Text(store.status?.summary ?? store.document?.status.summary ?? String(localized: "Connecting to the fleet…"))
                .font(.subheadline.weight(.medium)).foregroundStyle(.secondary).lineLimit(1)
                .contentTransition(.numericText())
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

    private var stack: some View {
        let ids = Array(store.orderedIDs.dropFirst(store.index).prefix(3))
        return ZStack(alignment: .top) {
            ForEach(ids.reversed(), id: \.self) { id in
                let depth = ids.firstIndex(of: id) ?? 0
                Group {
                    if let card = store.cards[id] {
                        ServiceHomeCard(card: card,
                                        actions: store.scope(state: \.actions, action: \.actions),
                                        attachments: store.scope(state: \.attachments, action: \.attachments),
                                        isFront: depth == 0, zoom: zoom,
                                        submit: { store.send(.submitSelected) },
                                        openReview: openReview)
                    } else {
                        ProgressView().frame(maxWidth: .infinity, minHeight: 300)
                            .background(Palette.card, in: .rect(cornerRadius: 34, style: .continuous))
                    }
                }
                .frame(maxHeight: depth == 0 ? nil : frontHeight, alignment: .top)
                .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height in
                    if depth == 0 { frontHeight = height }
                }
                .scaleEffect(1 - CGFloat(depth) * 0.05, anchor: .top)
                .offset(y: CGFloat(depth) * -11)
                .brightness(depth == 0 ? 0 : -0.04 * Double(depth))
                .offset(x: depth == 0 ? drag : 0)
                .opacity(depth == 0 ? 1 - Double(min(abs(drag) / 900, 0.35)) : 1)
                .zIndex(Double(3 - depth))
                .allowsHitTesting(depth == 0)
                .accessibilityHidden(depth != 0)
                .transition(reduceMotion ? .opacity : .opacity.combined(with: .scale(scale: 0.96, anchor: .top)))
            }
        }
        .padding(.top, CGFloat(max(0, ids.count - 1)) * 11)
        .simultaneousGesture(
            DragGesture(minimumDistance: 24)
                .onChanged { value in
                    guard abs(value.translation.width) > abs(value.translation.height) else { return }
                    drag = value.translation.width
                }
                .onEnded { value in
                    let projected = value.predictedEndTranslation.width
                    withAnimation(.spring(duration: 0.4, bounce: 0.35)) {
                        if projected < -90 { store.send(.move(1)) }
                        else if projected > 90 { store.send(.move(-1)) }
                        drag = 0
                    }
                }
        )
        .animation(.spring(duration: 0.5, bounce: 0.22), value: store.currentID)
    }

    private var empty: some View {
        VStack(spacing: 18) {
            Spacer()
            Image(systemName: "sunrise.fill")
                .font(.system(size: 64, weight: .light))
                .foregroundStyle(LinearGradient(colors: [Palette.dawn, Palette.ember], startPoint: .top, endPoint: .bottom))
                .symbolEffect(.breathe.pulse, options: .repeat(.continuous))
            Text(store.document?.empty?.text ?? String(localized: "Nothing is waiting for you"))
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
        Int((store.status ?? store.document?.status)?.counts.first(where: { $0.id == id })?.value ?? 0)
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

private struct HomeMachineBadge: View {
    let gauges: [AlondraViews.Gauge]
    var body: some View {
        HStack(spacing: 6) {
            Circle().fill(Palette.sage).frame(width: 8, height: 8)
                .shadow(color: Palette.sage.opacity(0.8), radius: 4)
            Image(systemName: "desktopcomputer").font(.footnote)
            ForEach(gauges.filter { $0.id != "disk" }, id: \.id) { gauge in
                HStack(spacing: 3) {
                    HomeReadingBar(value: gauge.value, tint: gauge.level.tint)
                    Text(gauge.text).font(.caption2.monospacedDigit().weight(.medium))
                        .foregroundStyle(gauge.level == .hot ? Palette.rose : Palette.ink.opacity(0.65))
                        .contentTransition(.numericText())
                }
            }
        }.foregroundStyle(Palette.ink.opacity(0.65))
    }
}

private struct HomeMachineDetail: View {
    let gauges: [AlondraViews.Gauge]
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Mac").font(.display(.title3, weight: .bold))
            ForEach(gauges, id: \.id) { gauge in
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    HomeReadingBar(value: gauge.value, tint: gauge.level.tint)
                    Text(gauge.detail ?? "\(gauge.label): \(gauge.text)")
                        .foregroundStyle(gauge.level == .hot ? Palette.rose : Palette.ink)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }.font(.subheadline).padding(20).frame(minWidth: 260, alignment: .leading)
    }
}

private struct HomeReadingBar: View {
    let value: Double
    let tint: Color
    var body: some View {
        Capsule().fill(Palette.ink.opacity(0.15)).frame(width: 22, height: 4)
            .overlay(alignment: .leading) {
                Capsule().fill(tint).frame(width: 22 * max(0.06, min(value, 1)))
            }
    }
}

private extension Level {
    var tint: Color {
        switch self {
        case .calm: Palette.sky
        case .warm: Palette.dawn
        case .hot: Palette.rose
        }
    }
}
