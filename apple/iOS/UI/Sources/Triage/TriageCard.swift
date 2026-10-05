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
    @Environment(\.dynamicTypeSize) private var typeSize

    private var kind: Kind { Kind(card) }
    private var tint: Color { kind.tint }
    private var stamp: String? { store.stamp }

    var body: some View { compositionSheet(changedAlert(confirmationAlert(shell))) }

    private var shell: some View {
        Color.clear
            .overlay(alignment: .topLeading) {
                content.padding(.horizontal, 20).padding(.vertical, 20)
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
        .sensoryFeedback(.success, trigger: stamp) { _, new in new != nil }
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

    private struct Fit {
        var why = true
        var meta = true
        var chips = true
        var row = true
        var state = true
        var reply = true

        static let compact = Fit(why: false, meta: false, chips: false, row: false, state: false, reply: false)
    }

    private var content: some View {
        ViewThatFits(in: .vertical) {
            layout(Fit())
            layout(Fit(meta: false))
            layout(Fit(why: false, meta: false))
            layout(Fit(why: false, meta: false, chips: false))
            layout(Fit(why: false, meta: false, chips: false, row: false))
            layout(Fit(why: false, meta: false, chips: false, row: false, state: false))
            layout(Fit.compact)
            layout(Fit.compact).dynamicTypeSize(...DynamicTypeSize.accessibility2)
            layout(Fit.compact).dynamicTypeSize(...DynamicTypeSize.xxxLarge)
        }
    }

    private func layout(_ fit: Fit) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            header(showsState: fit.state)
            summary(why: fit.why, meta: fit.meta)
            if fit.row, let row = card.contentRow {
                CardContentRow(row: row, tint: tint) { openDetail() }
            }
            if fit.chips {
                BlockChips(blocks: card.blocks, blockedBy: card.blockedBy, open: handleLink)
            }
            Spacer(minLength: 0)
            if let error = store.actions.error { Text(error).font(.caption).foregroundStyle(Palette.rose) }
            buttons
            if fit.reply, let reply = store.replyAction {
                AttachmentStrip(store: store.scope(state: \.attachments, action: \.attachments))
                composer(reply)
            }
        }
    }

    private func confirmationAlert<Content: View>(_ view: Content) -> some View {
        view.alert(store.actions.confirmation?.title ?? "", isPresented: Binding(
            get: { store.actions.confirmation != nil }, set: { if !$0 { store.send(.actions(.cancelConfirmation)) } }
        )) {
            Button(store.actions.confirmation?.button ?? String(localized: "Continue")) { store.send(.actions(.confirm)) }
            Button("Cancel", role: .cancel) { store.send(.actions(.cancelConfirmation)) }
        } message: { Text(store.actions.confirmation?.message ?? "") }
    }

    private func changedAlert<Content: View>(_ view: Content) -> some View {
        view.alert("This item changed", isPresented: Binding(
            get: { store.actions.changed != nil }, set: { if !$0 { store.send(.actions(.dismiss)) } }
        )) {
            Button(store.actions.changed?.button ?? String(localized: "Continue")) { store.send(.actions(.sendChangedAction)) }
            Button("Cancel", role: .cancel) { store.send(.actions(.dismiss)) }
        } message: { Text(store.actions.error ?? "") }
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
                HStack(spacing: 8) { KindBadge(kind: kind); Spacer(minLength: 4); trailing }
                if showsState, let state = card.state { stateText(state) }
            }
        }
    }

    private func badges(showsState: Bool) -> some View {
        HStack(spacing: 8) {
            KindBadge(kind: kind)
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
            }
        }
    }

    private func summary(why: Bool, meta: Bool) -> some View {
        let large = typeSize.isAccessibilitySize
        return VStack(alignment: .leading, spacing: 8) {
            Text(card.need ?? card.title).font(.display(.title2, weight: .bold)).foregroundStyle(Palette.ink)
                .lineLimit(large ? 2 : 3)
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
    }

    @ViewBuilder private func actionButton(_ action: AlondraContract.Action) -> some View {
        VStack(spacing: 4) {
            if action.presentation == .menu, !action.options.isEmpty {
                Menu {
                    ForEach(action.options, id: \.id) { option in
                        Button(option.label) { store.send(.menuOptionPicked(action, optionID: option.id)) }
                    }
                } label: {
                    GlassBarLabel(action.label)
                }
                .menuStyle(.button)
                .glassRole(action.cardRole)
                .disabled(!action.enabled)
            } else {
                Button { perform(action) } label: { GlassBarLabel(action.label).multilineTextAlignment(.center) }
                    .glassRole(action.cardRole)
                    .disabled(!action.enabled)
            }
            if !action.enabled, let reason = action.disabledReason {
                Text(reason).font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.center)
                    .frame(maxWidth: .infinity)
            }
        }
    }

    private var moreMenu: some View {
        Menu {
            ForEach(Array(store.menu.enumerated()), id: \.offset) { _, action in
                Button(action.label) { perform(action) }.disabled(!action.enabled)
            }
        } label: {
            Image(systemName: "ellipsis.circle")
        }
        .menuStyle(.button).buttonStyle(.glass)
        .accessibilityLabel("More actions")
    }

    private func perform(_ action: AlondraContract.Action) {
        if case .url(let url) = action.link, let destination = URL(string: url) {
            openURL(destination)
            return
        }
        store.send(.actionTapped(action))
    }

    private func composer(_ action: AlondraContract.Action) -> some View {
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
                .lineLimit(1...5).focused($replyFocused)
                .padding(.leading, 16).padding(.vertical, 12)
            Button {
                replyFocused = false
                store.send(.replySent(action))
            } label: {
                Image(systemName: "arrow.up").font(.body.weight(.bold)).foregroundStyle(Palette.onAccent)
                    .frame(width: 36, height: 36).background(empty ? Palette.slate.opacity(0.5) : tint, in: .circle)
            }
            .buttonStyle(PressStyle())
            .disabled(empty || !action.enabled)
            .padding(4)
            .accessibilityLabel("Send")
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
