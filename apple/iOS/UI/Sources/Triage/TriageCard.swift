import AlondraDesign
import AlondraAttachments
import AlondraContract
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// One card of Waiting for You: the 34 pt shell, what the card needs of you, its one line of
/// content, the buttons under it, the reply field, and the stamp that covers it the moment an
/// answer is sent. The card is always as tall as the stack makes it and nothing inside scrolls: the
/// content adapts to the card. The buttons and the reply field keep their place at the bottom; the
/// title, why and meta give way first, and whatever does not fit lives in the detail.
struct TriageCard: View {
    @Bindable var store: StoreOf<TriageCardFeature>
    let card: Card
    let isFront: Bool
    let zoom: Namespace.ID
    let onSkip: (() -> Void)?
    @Environment(\.openURL) private var openURL
    @FocusState private var replyFocused: Bool
    @FocusState private var roomFocused: Bool
    @State private var room: ComposerRoom?
    @State private var choosing: AlondraContract.Action?
    @State private var showsMore = false
    @Environment(\.dynamicTypeSize) private var typeSize

    private var kind: Kind { Kind(card) }
    private var tint: Color { kind.tint }
    private var stamp: String? { store.stamp }

    var body: some View { compositionSheet(shell.actionDialogs(store.scope(state: \.actions, action: \.actions))) }

    private var shell: some View {
        Color.clear
            .overlay(alignment: .topLeading) {
                content.padding(.horizontal, 20).padding(.vertical, 20)
                    .allowsHitTesting(stamp == nil)
                    .accessibilityHidden(stamp != nil)
            }
        .blur(radius: stamp == nil ? 0 : 10)
        .opacity(stamp == nil ? 1 : 0.4)
        .background { surface }
        .clipShape(.rect(cornerRadius: 34, style: .continuous))
        .matchedTransitionSource(id: card.id, in: zoom)
        .overlay { RoundedRectangle(cornerRadius: 34, style: .continuous).strokeBorder(Palette.hairline) }
        .overlay { stampOverlay }
        .shadow(color: .black.opacity(isFront ? 0.22 : 0.08), radius: isFront ? 24 : 8, y: isFront ? 14 : 4)
        .scaleEffect(stamp == nil ? 1 : 1.02)
        .animation(.spring(duration: 0.35, bounce: 0.4), value: stamp)
        .accessibilityAction(named: "Open") { openDetail() }
        .accessibilityActions {
            if let onSkip { Button("Leave for Later", action: onSkip) }
        }
    }

    private var surface: some View {
        ZStack(alignment: .top) {
            Palette.card
            LinearGradient(colors: [tint.opacity(0.16), tint.opacity(0)], startPoint: .top, endPoint: .bottom)
                .frame(height: 180)
        }
    }

    private struct Fit: Equatable {
        var why = true
        var meta = true
        var chips = true
        var row = true
        var state = true
        var reply = true

        static let compact = Fit(why: false, meta: false, chips: false, row: false, state: false, reply: false)
    }

    /// Each layout only keeps room for the reply field; the field itself is drawn once, over that
    /// room. A field inside the layouts would be rebuilt, and lose the keyboard, whenever the
    /// keyboard shrinks the card and another layout is picked.
    private var content: some View {
        ViewThatFits(in: .vertical) {
            Layout(card: self, fit: Fit())
            Layout(card: self, fit: Fit(meta: false))
            Layout(card: self, fit: Fit(why: false, meta: false))
            Layout(card: self, fit: Fit(why: false, meta: false, chips: false))
            Layout(card: self, fit: Fit(why: false, meta: false, chips: false, row: false))
            Layout(card: self, fit: Fit(why: false, meta: false, chips: false, row: false, state: false))
            Layout(card: self, fit: .compact)
            Layout(card: self, fit: .compact).dynamicTypeSize(...DynamicTypeSize.accessibility2)
            Layout(card: self, fit: .compact).dynamicTypeSize(...DynamicTypeSize.xxxLarge)
        }
        .coordinateSpace(.named(ComposerRoom.space))
        .overlay(alignment: room == nil ? .bottomLeading : .topLeading) {
            if let reply = store.replyAction { placedComposer(reply) }
        }
        .disabled(store.actions.inFlight && store.actions.stamp == nil)
    }

    /// Where the picked layout keeps room for the reply field, and the text size it has there.
    private struct ComposerRoom: Equatable {
        static let space = "composer-room"
        let fit: Fit
        let frame: CGRect
        let typeSize: DynamicTypeSize

        struct Placeholder<Content: View>: View {
            let fit: Fit
            let content: Content
            @Binding var room: ComposerRoom?
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

    /// With no room in the picked layout the field stays over the bottom of the card while it has
    /// the keyboard, so the person keeps writing; once they leave it, the buttons show again.
    private func placedComposer(_ reply: AlondraContract.Action) -> some View {
        let shown = (room != nil || replyFocused) && stamp == nil
        return composer(reply, live: true)
            .dynamicTypeSize(room?.typeSize ?? typeSize)
            .frame(width: room?.frame.width)
            .offset(x: room?.frame.minX ?? 0, y: room?.frame.minY ?? 0)
            .opacity(shown ? 1 : 0)
            .allowsHitTesting(shown)
            .accessibilityHidden(!shown)
    }

    /// One candidate of the card's ViewThatFits as a view of its own. Built inline, every candidate
    /// is a value as large as the whole card and ViewThatFits holds all nine on the stack at once:
    /// 1.1 MB, more than the iPhone's 1 MB main thread stack, so build 47 crashed on launch.
    private struct Layout: View {
        let card: TriageCard
        let fit: Fit

        var body: some View { card.layout(fit) }
    }

    private func layout(_ fit: Fit) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            header(showsState: fit.state)
            summary(why: fit.why, meta: fit.meta)
            if fit.row, let row = card.contentRow {
                CardContentRow(row: row, tint: tint) { openDetail() }
            }
            if fit.chips, !card.badges.isEmpty {
                HStack(spacing: 10) { ForEach(card.badges, id: \.text) { stateText($0) } }
            }
            if fit.chips {
                BlockChips(blocks: card.blocks, blockedBy: card.blockedBy, open: handleLink)
            }
            Spacer(minLength: 0)
            if let error = store.actions.error { Text(error).font(.caption).foregroundStyle(Palette.rose) }
            buttons
            if fit.reply, let reply = store.replyAction {
                AttachmentStrip(store: store.scope(state: \.attachments, action: \.attachments))
                ComposerRoom.Placeholder(fit: fit, content: composer(reply, live: false), room: $room)
            }
        }
    }

    private func compositionSheet<Content: View>(_ view: Content) -> some View {
        view.sheet(item: Binding(get: { store.composition }, set: { if $0 == nil { store.send(.compositionCancelled) } })) { composition in
            CompositionSheet(
                composition: composition, tint: tint,
                reasonChanged: { store.send(.reasonChanged($0)) }, noteChanged: { store.send(.noteChanged($0)) },
                cancel: { store.send(.compositionCancelled) }, send: { store.send(.compositionConfirmed) }
            )
        }
    }

    private func header(showsState: Bool) -> some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 8) { badges(showsState: showsState); Spacer(minLength: 4); trailing }
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) { KindBadge(kind: kind, title: card.eyebrow?.text); Spacer(minLength: 4); trailing }
                if showsState, let state = card.state { stateText(state) }
            }
        }
    }

    private func badges(showsState: Bool) -> some View {
        HStack(spacing: 8) {
            KindBadge(kind: kind, title: card.eyebrow?.text)
            if showsState, let state = card.state { stateText(state) }
        }
    }

    private func stateText(_ state: AlondraContract.Label) -> some View {
        Text(state.text).font(.caption.weight(.medium)).foregroundStyle(Self.color(state.tone)).lineLimit(2)
    }

    private var trailing: some View {
        HStack(spacing: 8) {
            if let at = card.timestamps.first?.at {
                Text(relativeSince(at) ?? at).font(.footnote).foregroundStyle(.secondary).lineLimit(1).fixedSize()
            }
            if let onSkip {
                Button(action: onSkip) {
                    Image(systemName: "arrow.uturn.forward")
                        .font(.footnote.weight(.semibold))
                        .frame(width: 30, height: 30)
                        .background(Palette.hairline, in: .circle)
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .accessibilityLabel("Leave for Later")
                .accessibilityIdentifier("triage.later.\(card.id)")
            }
        }
    }

    private func summary(why: Bool, meta: Bool) -> some View {
        let large = typeSize.isAccessibilitySize
        return VStack(alignment: .leading, spacing: 8) {
            Text(card.need ?? card.title).font(.display(.title2, weight: .bold)).foregroundStyle(Palette.ink)
                .lineLimit(large ? 2 : 3)
                .accessibilityIdentifier("triage.need.\(card.id)")
            if why, let why = card.why {
                Text(why).font(.subheadline).foregroundStyle(.secondary).lineLimit(large ? 2 : 4)
            }
            if meta, let meta = card.meta {
                Text(meta).font(.footnote.weight(.medium)).foregroundStyle(.secondary).lineLimit(large ? 1 : 2)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(.rect)
        .onTapGesture { openDetail() }
    }

    private var buttons: some View {
        GlassEffectContainer(spacing: 8) {
            VStack(spacing: 8) {
                ForEach(Array(store.buttons.dropLast().enumerated()), id: \.offset) { _, action in
                    actionButton(action)
                }
                HStack(alignment: .top, spacing: 8) {
                    if let last = store.buttons.last { actionButton(last) }
                    if !store.menu.isEmpty { moreMenu }
                }
            }
            .controlSize(.large)
            .buttonBorderShape(.capsule)
            .font(.headline)
        }
        .opacity(stamp == nil ? 1 : 0)
    }

    /// Buttons with options, and More, ask in a dialog anchored to them, never a menu: iOS 27 draws
    /// a closing menu's button above everything else while it folds back, over the stamp.
    @ViewBuilder private func actionButton(_ action: AlondraContract.Action) -> some View {
        VStack(spacing: 4) {
            if action.presentation == .menu, !action.options.isEmpty {
                Button { choosing = action } label: { GlassBarLabel(action.label) }
                    .glassRole(action.cardRole)
                    .accessibilityIdentifier(actionIdentifier(action))
                    .confirmationDialog(
                        action.label,
                        isPresented: Binding(get: { choosing == action }, set: { if !$0 { choosing = nil } }),
                        titleVisibility: .hidden
                    ) {
                        ForEach(action.options, id: \.id) { option in
                            Button(option.label) { store.send(.menuOptionPicked(action, optionID: option.id)) }
                        }
                    }
            } else {
                Button { perform(action) } label: { GlassBarLabel(action.label).multilineTextAlignment(.center) }
                    .glassRole(action.cardRole)
                    .accessibilityIdentifier(actionIdentifier(action))
            }
            AllowAlwaysDescription(action: action)
        }
    }

    private func actionIdentifier(_ action: AlondraContract.Action) -> String {
        "triage.action.\(card.id).\(action.optionId ?? action.label)"
    }

    private var moreMenu: some View {
        Button { showsMore = true } label: { Image(systemName: "ellipsis.circle") }
            .buttonStyle(.glass)
            .accessibilityLabel("More actions")
            .confirmationDialog("More actions", isPresented: $showsMore, titleVisibility: .hidden) {
                ForEach(Array(store.menu.enumerated()), id: \.offset) { _, action in
                    Button(action.label) { perform(action) }
                }
            }
    }

    private func perform(_ action: AlondraContract.Action) {
        if case .url(let url) = action.link, let destination = URL(string: url) {
            openURL(destination)
            return
        }
        store.send(.actionTapped(action))
    }

    private func composer(_ action: AlondraContract.Action, live: Bool) -> some View {
        let placeholder: String = {
            if case .text(let placeholder, _, _) = action.input, let placeholder { return placeholder }
            return String(localized: "Reply to the node…")
        }()
        let empty = store.replyDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        return HStack(alignment: .bottom, spacing: 8) {
            if action.attachments {
                AttachButton(store: store.scope(state: \.attachments, action: \.attachments)).padding(.leading, 6).padding(.bottom, 4)
            }
            TextField(placeholder, text: Binding(
                get: { store.replyDraft }, set: { store.send(.replyChanged($0)) }
            ), axis: .vertical)
                .lineLimit(1...5).focused(live ? $replyFocused : $roomFocused)
                .padding(.leading, 16).padding(.vertical, 12)
                .accessibilityIdentifier("triage.reply.\(card.id)")
            Button {
                replyFocused = false
                store.send(.replySent(action))
            } label: {
                Image(systemName: "arrow.up").font(.body.weight(.bold)).foregroundStyle(Palette.onAccent)
                    .frame(width: 36, height: 36).background(empty ? Palette.slate.opacity(0.5) : tint, in: .circle)
            }
            .buttonStyle(PressStyle())
            .disabled(empty)
            .padding(4)
            .accessibilityLabel("Send")
            .accessibilityIdentifier("triage.send.\(card.id)")
        }
        .background(Palette.cardRaised, in: .rect(cornerRadius: 22, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 22, style: .continuous).strokeBorder(Palette.hairline))
    }

    @ViewBuilder private var stampOverlay: some View {
        if let stamp {
            ZStack {
                RadialGradient(colors: [tint.opacity(0.35), tint.opacity(0.05)], center: .center, startRadius: 10, endRadius: 260)
                    .clipShape(.rect(cornerRadius: 34, style: .continuous))
                VStack(spacing: 10) {
                    Image(systemName: "checkmark").font(.system(size: 44, weight: .bold))
                        .foregroundStyle(Palette.onAccent).frame(width: 92, height: 92)
                        .background(tint, in: .circle).shadow(color: tint.opacity(0.5), radius: 20)
                        .symbolEffect(.bounce, value: stamp)
                    Text(stamp).font(.display(.title2, weight: .bold)).foregroundStyle(Palette.ink)
                }
            }.transition(.opacity)
        }
    }

    private func handleLink(_ link: AlondraContract.Link) {
        switch link {
        case let .view(key): store.send(.opened(ViewKey(rawValue: key)))
        case let .url(url): if let destination = URL(string: url) { openURL(destination) }
        case .local: break
        }
    }

    private func openDetail() { store.send(.opened(.card(card.id))) }

    private static func color(_ tone: Tone) -> Color {
        switch tone {
        case .success: Palette.sage
        case .warning: Palette.gold
        case .critical: Palette.rose
        case .accent, .yourTurn: Palette.dawn
        case .info: Palette.sky
        case .neutral: Palette.slate
        }
    }
}

struct ArtifactLink: View {
    let url: URL
    let tint: Color

    var body: some View {
        Link(destination: url) {
            HStack(spacing: 12) {
                Image(systemName: "safari").font(.title3).foregroundStyle(tint)
                VStack(alignment: .leading, spacing: 2) {
                    Text(Artifacts.openInBrowserLabel).font(.headline).foregroundStyle(Palette.ink)
                    Text(url.absoluteString).font(.caption).lineLimit(1).truncationMode(.middle)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Image(systemName: "arrow.up.right").font(.footnote.weight(.bold)).foregroundStyle(.tertiary)
            }
            .padding(14)
            .background(tint.opacity(0.10), in: .rect(cornerRadius: 18, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous).strokeBorder(tint.opacity(0.25)))
            .contentShape(.rect(cornerRadius: 18))
        }
        .buttonStyle(.plain)
    }
}

struct ArtifactLinks: View {
    let text: String
    let tint: Color

    var body: some View {
        ForEach(Artifacts.urls(in: text), id: \.self) { url in
            ArtifactLink(url: url, tint: tint).padding(.bottom, 12)
        }
    }
}
