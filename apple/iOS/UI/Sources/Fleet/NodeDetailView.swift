import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// A node: its state and folder, its tasks, what it is doing right now, what it needs, what it
/// said last, and a composer to write to it.
public struct NodeDetailView: View {
    @Bindable var store: StoreOf<NodeDetailFeature>

    public init(store: StoreOf<NodeDetailFeature>) { self.store = store }

    public var body: some View {
        Group {
            if let detail = store.detail {
                content(detail)
            } else {
                placeholder
            }
        }
        .background(DawnBackground(glow: Palette.sky))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .toolbar {
            if let close = store.detail?.close {
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        if let interrupt = store.detail?.interrupt {
                            Button { store.send(.perform(interrupt)) } label: { Label(interrupt.label, systemImage: "stop.circle") }
                        }
                        Button(role: .destructive) { store.send(.perform(close)) } label: {
                            Label("Close Node", systemImage: "xmark.bin")
                        }
                    } label: {
                        Label("More", systemImage: "ellipsis")
                    }
                }
            }
        }
        .task { await store.send(.task).finish() }
        .actionDialogs(store.scope(state: \.actions, action: \.actions))
        .sensoryFeedback(.success, trigger: store.composer.stamp) { _, new in new != nil }
    }

    @ViewBuilder private var placeholder: some View {
        switch store.screen.phase {
        case .missing:
            ContentUnavailableView("This node is gone", systemImage: "questionmark.circle")
        case .failed(let message):
            ContentUnavailableView {
                Label(message, systemImage: "exclamationmark.triangle")
            } actions: {
                Button("Try Again") { store.send(.screen(.retry)) }
            }
        case .loading, .loaded:
            ProgressView().tint(Palette.dawn).frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func content(_ detail: NodeDetail) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                header(detail)
                if !detail.tasks.isEmpty { tasks(detail.tasks) }
                if !detail.now.isEmpty {
                    NodeSection(title: "Right now", symbol: "wrench.and.screwdriver") {
                        VStack(alignment: .leading, spacing: 12) {
                            ForEach(detail.now) { ThreadLine(activity: $0) }
                        }
                    }
                }
                if !detail.background.isEmpty {
                    NodeSection(title: "Running in the background", symbol: "square.stack.3d.up") {
                        VStack(alignment: .leading, spacing: 12) {
                            ForEach(detail.background) { BackgroundRow(activity: $0) }
                        }
                    }
                }
                if let failure = detail.failure {
                    NodeSection(title: "What went wrong", symbol: "exclamationmark.triangle") { MarkdownView(markdown: failure) }
                }
                if let needs = detail.needs {
                    NodeSection(title: "Needs", symbol: "hand.raised") { MarkdownView(markdown: needs) }
                }
                if !detail.waiting.isEmpty { waiting(detail.waiting) }
                if let summary = detail.summary {
                    NodeSection(title: "Summary", symbol: "text.quote") { MarkdownView(markdown: summary) }
                }
                if let message = detail.lastMessage {
                    NodeSection(title: "What it said last", symbol: "text.quote", at: detail.lastMessageAt,
                                format: detail.lastMessageFormat) { MarkdownView(markdown: message) }
                }
                if !detail.orchestratorActions.isEmpty { orchestratorActions(detail.orchestratorActions) }
            }
            .padding(20)
            .animation(.smooth, value: detail.orchestratorActions.isEmpty)
        }
        .scrollDismissesKeyboard(.interactively)
        .safeAreaBar(edge: .bottom) { if detail.message != nil { composer(detail) } }
    }

    private func header(_ detail: NodeDetail) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                let tint = store.isOrchestrator ? Palette.sky : NodeStyle.tint(tone: detail.stateTone)
                Glyph(symbol: store.isOrchestrator ? "point.3.connected.trianglepath.dotted" : NodeStyle.symbol(tone: detail.stateTone),
                      tint: tint, size: .small)
                if let state = detail.state {
                    Text(state.prefix(1).uppercased() + state.dropFirst())
                        .font(.subheadline.weight(.semibold)).foregroundStyle(tint)
                }
                if let since = detail.since {
                    HStack(spacing: 4) {
                        Text("since")
                        TimeText(raw: since)
                    }
                    .font(.subheadline).foregroundStyle(.secondary)
                }
            }
            Text(detail.title)
                .font(.display(.largeTitle, weight: .bold)).foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
            if let folder = detail.folder {
                Text(folder.replacing(/^\/Users\/[^\/]+\//, with: "~/"))
                    .font(.mono(.caption)).foregroundStyle(.secondary).lineLimit(2)
            }
            if !detail.readings.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(detail.readings.enumerated()), id: \.offset) { _, reading in
                        HStack(alignment: .firstTextBaseline, spacing: 6) {
                            Text(reading.label).foregroundStyle(.secondary)
                            Text(reading.value)
                                .foregroundStyle(reading.tone.map { NodeStyle.tint(tone: $0) } ?? Palette.ink)
                                .lineLimit(2)
                        }
                        .font(.caption)
                    }
                }
            }
        }
    }

    private func tasks(_ rows: [Row]) -> some View {
        NodeSection(title: "Tasks", symbol: "checklist") {
            VStack(alignment: .leading, spacing: 12) {
                ForEach(rows, id: \.id) { row in
                    Button { if let link = row.link { store.send(.follow(link)) } } label: {
                        HStack(spacing: 10) {
                            Glyph(symbol: "circle.dashed", tint: Palette.dawn, size: .small)
                            Text(row.title).font(.subheadline.weight(.semibold)).foregroundStyle(Palette.ink)
                                .multilineTextAlignment(.leading)
                            Spacer(minLength: 0)
                            Image(systemName: "chevron.right").font(.caption.weight(.bold)).foregroundStyle(.tertiary)
                        }
                        .contentShape(.rect)
                    }
                    .buttonStyle(PressStyle())
                }
            }
        }
    }

    private func waiting(_ cards: [AlondraContract.Card]) -> some View {
        NodeSection(title: "Waiting for You", symbol: "tray.full") {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(cards, id: \.id) { card in
                    Button { if let link = card.link { store.send(.follow(link)) } } label: {
                        HStack(spacing: 10) {
                            KindBadge(kind: Kind(card))
                            Text(card.title).font(.subheadline.weight(.medium)).foregroundStyle(Palette.ink)
                                .lineLimit(2).multilineTextAlignment(.leading)
                            Spacer(minLength: 0)
                            Image(systemName: "chevron.right").font(.caption.weight(.bold)).foregroundStyle(.tertiary)
                        }
                        .contentShape(.rect)
                    }
                    .buttonStyle(PressStyle())
                }
            }
        }
    }

    private func orchestratorActions(_ actions: [WireAction]) -> some View {
        VStack(spacing: 8) {
            ForEach(actions, id: \.id) { action in
                Button(role: action.style == .destructive ? .destructive : nil) {
                    store.send(.perform(action))
                } label: {
                    Text(action.label).frame(maxWidth: .infinity)
                }
                .buttonStyle(.glass)
                .disabled(!action.enabled)
            }
        }
        .controlSize(.large).buttonBorderShape(.capsule)
    }

    private func composer(_ detail: NodeDetail) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            AttachmentStrip(store: store.scope(state: \.composer.attachmentPicker, action: \.composer.attachmentPicker))
            HStack(alignment: .bottom, spacing: 10) {
                HStack(alignment: .bottom, spacing: 4) {
                    AttachButton(store: store.scope(state: \.composer.attachmentPicker, action: \.composer.attachmentPicker))
                        .padding(.leading, 6).padding(.bottom, 4)
                    TextField(composerPlaceholder(detail.message), text: Binding(get: { store.draft }, set: { store.send(.draftChanged($0)) }),
                              axis: .vertical)
                        .lineLimit(1...5)
                        .padding(.trailing, 16).padding(.vertical, 12)
                }
                .glassEffect(.regular.interactive(), in: .rect(cornerRadius: 22))
                Button { store.send(.send) } label: {
                    Image(systemName: "arrow.up")
                        .font(.body.weight(.bold)).frame(width: 26, height: 26)
                }
                .buttonStyle(.glassProminent).buttonBorderShape(.circle).tint(Palette.dawn)
                .disabled(!store.canSend)
                .accessibilityLabel("Send")
            }
        }
        .padding(.horizontal, 16).padding(.vertical, 8)
    }

    private func composerPlaceholder(_ action: WireAction?) -> String {
        if case .text(let placeholder, _, _)? = action?.input, let placeholder { return placeholder }
        return String(localized: "Write a message")
    }
}

private struct NodeSection<Content: View>: View {
    let title: LocalizedStringKey
    let symbol: String
    var at: String?
    var format = TimeFormat.relative
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label(title, systemImage: symbol)
                    .font(.footnote.weight(.bold)).textCase(.uppercase).kerning(0.6).foregroundStyle(.secondary)
                Spacer()
                if let at { TimeText(raw: at, format: format).font(.caption).foregroundStyle(.tertiary) }
            }
            content
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(16)
                .background(Palette.card.opacity(0.9), in: .rect(cornerRadius: 20, style: .continuous))
        }
    }
}

/// One thing the node is doing now, with the task it belongs to.
private struct ThreadLine: View {
    let activity: NodeDetail.Activity

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "terminal").font(.footnote).foregroundStyle(.secondary).frame(width: 20)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Text(activity.title).font(.mono(.footnote)).foregroundStyle(Palette.sky).lineLimit(2)
                    Spacer(minLength: 4)
                    if let since = activity.since { TimeText(raw: since).font(.caption).foregroundStyle(.tertiary) }
                }
                if let detail = activity.detail {
                    Text(detail).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
            }
        }
    }
}
