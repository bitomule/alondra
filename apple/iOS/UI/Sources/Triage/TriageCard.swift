import AlondraDesign
import AlondraAttachments
import AlondraContract
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// One card of Waiting for You: the 34 pt shell, the scrolling body, the pinned controls and the
/// stamp that covers it once the server has applied what was sent.
struct TriageCard: View {
    @Bindable var store: StoreOf<TriageCardFeature>
    let card: Card
    let isFront: Bool
    let zoom: Namespace.ID
    @Environment(\.openURL) private var openURL
    @State private var contentHeight: CGFloat?
    @FocusState private var replyFocused: Bool
    @Environment(\.dynamicTypeSize) private var typeSize

    private var kind: Kind { Kind(card) }
    private var tint: Color { kind.tint }
    private var actionTint: Color { kind.tint == Palette.slate ? Palette.dawn : kind.tint }
    private var replyAction: AlondraContract.Action? { store.replyAction }
    private var selected: AlondraContract.Action? { store.selected }
    private func inputPlaceholder(_ action: AlondraContract.Action) -> String {
        if card.icon == .pullRequest { return String(localized: "What should change?") }
        let picked = selected?.id == action.id ? action.options.first { $0.id == store.actions.selectedOptionID } : nil
        if case let .text(value, _, _) = picked?.input ?? action.input {
            return value ?? String(localized: "Write your answer")
        }
        return String(localized: "Write your answer")
    }
    private var stamp: String? { store.stamp }
    private var isQuestion: Bool { card.icon == .question || card.icon == .action || card.icon == .warning }
    private var isDocumentReview: Bool { CardActionPresentation.isDocumentReview(card) }
    private func verb(_ action: AlondraContract.Action) -> String {
        action.id?.split(separator: ":", maxSplits: 1).first.map(String.init) ?? ""
    }
    private func controlRank(_ action: AlondraContract.Action) -> Int {
        switch (card.icon, verb(action)) {
        case (.question, "ctx"), (.action, "ctx"): return 1
        case (.document, "changes"), (.delivery, "changes"): return 1
        case (.document, "accept"), (.delivery, "accept"): return 2
        case (.pullRequest, "ask_pr"): return 1
        case (.pullRequest, "merge"), (.pullRequest, "conflicts"): return 2
        default: return CardActionPresentation.isOpenOnGitHub(action) ? 5 : 4
        }
    }
    private var controlActions: [AlondraContract.Action] {
        card.actions.filter { CardActionPresentation.showsAsControl($0, in: card, reply: replyAction) }
            .sorted { controlRank($0) < controlRank($1) }
    }

    var body: some View {
        layout
            .blur(radius: stamp == nil ? 0 : 10)
            .opacity(stamp == nil ? 1 : 0.4)
            .frame(maxWidth: .infinity)
            .background {
                ZStack(alignment: .top) {
                    Palette.card
                    LinearGradient(colors: [tint.opacity(0.16), tint.opacity(0)], startPoint: .top, endPoint: .bottom)
                        .frame(height: 180)
                }
            }
            .clipShape(.rect(cornerRadius: 34, style: .continuous))
            .overlay { RoundedRectangle(cornerRadius: 34, style: .continuous).strokeBorder(Palette.hairline) }
            .overlay { stampOverlay }
            .shadow(color: .black.opacity(isFront ? 0.22 : 0.08), radius: isFront ? 24 : 8, y: isFront ? 14 : 4)
            .scaleEffect(stamp == nil ? 1 : 1.02)
            .animation(.spring(duration: 0.35, bounce: 0.4), value: stamp)
            .alert(store.actions.confirmation?.title ?? "", isPresented: Binding(
                get: { store.actions.confirmation != nil }, set: { if !$0 { store.send(.actions(.cancelConfirmation)) } }
            )) {
                Button(store.actions.confirmation?.button ?? String(localized: "Continue")) { store.send(.actions(.confirm)) }
                Button("Cancel", role: .cancel) { store.send(.actions(.cancelConfirmation)) }
            } message: { Text(store.actions.confirmation?.message ?? "") }
            .alert("This item changed", isPresented: Binding(
                get: { store.actions.changed != nil }, set: { if !$0 { store.send(.actions(.dismiss)) } }
            )) {
                Button(store.actions.changed?.button ?? String(localized: "Continue")) { store.send(.actions(.sendChangedAction)) }
                Button("Cancel", role: .cancel) { store.send(.actions(.dismiss)) }
            } message: { Text(store.actions.error ?? "") }
            .alert("What do you need to decide?", isPresented: Binding(
                get: { store.contextRequest != nil }, set: { if !$0 { store.send(.contextCancelled) } }
            )) {
                TextField("Optional", text: Binding(
                    get: { store.contextRequest?.note ?? "" }, set: { store.send(.contextNoteChanged($0)) }
                ))
                Button("Cancel", role: .cancel) { store.send(.contextCancelled) }
                Button("Ask for Context") { store.send(.contextConfirmed) }
            } message: {
                Text("The node will explain and ask again.")
            }
    }

    @ViewBuilder private var layout: some View {
        if typeSize.isAccessibilitySize {
            ScrollView { VStack(spacing: 0) { content; controls.padding(.horizontal, 16).padding(.bottom, 16) } }
                .scrollBounceBehavior(.basedOnSize)
                .scrollDismissesKeyboard(.interactively)
        } else {
            VStack(spacing: 0) {
                ScrollView { content }
                    .scrollBounceBehavior(.basedOnSize)
                    .scrollDismissesKeyboard(.interactively)
                    .onScrollGeometryChange(for: CGFloat.self) { $0.contentSize.height } action: { _, height in contentHeight = height }
                    .frame(maxHeight: contentHeight ?? .infinity)
                    .mask {
                        VStack(spacing: 0) {
                            Color.black
                            LinearGradient(colors: [.black, .black.opacity(0)], startPoint: .top, endPoint: .bottom).frame(height: 22)
                        }
                    }
                controls.padding(.horizontal, 16).padding(.bottom, 16).padding(.top, 4)
            }
        }
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 8) {
                KindBadge(kind: kind)
                Spacer()
                if let at = card.timestamps.first?.at {
                    Text(relativeStamp(at)).font(.footnote).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            Text(card.title).font(.display(.title2, weight: .bold)).foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
            if let subtitle = card.subtitle { Text(subtitle).font(.subheadline).foregroundStyle(.secondary) }
            if let node = card.context.first(where: { $0.icon == .node }) {
                contextNavigation(node) {
                HStack(spacing: 8) {
                    Image(systemName: "circle.fill")
                        .font(.caption2.weight(.bold)).foregroundStyle(SidebarTint.node(node.state?.tone))
                        .frame(width: 22, height: 22)
                        .background(SidebarTint.node(node.state?.tone).opacity(0.15), in: .circle)
                        .accessibilityLabel(Text(node.state?.text ?? ""))
                    Text(node.text).font(.mono(.subheadline, weight: .medium)).foregroundStyle(Palette.ink.opacity(0.85))
                    if let chip = node.chip {
                        Text(chip.text).font(.caption.weight(.medium)).foregroundStyle(.secondary)
                            .padding(.horizontal, 8).padding(.vertical, 3)
                            .background(Palette.hairline, in: .capsule)
                    }
                }
                .lineLimit(1)
                }
            }
            if let task = card.context.first(where: { $0.icon == .task }) {
                contextNavigation(task) {
                Label(task.text, systemImage: "checklist")
                    .font(.subheadline).foregroundStyle(.secondary).lineLimit(1)
                    .accessibilityLabel(Text("Task: \(task.text)"))
                }
            }
            if let mission = card.context.first(where: { $0.icon == .mission }) {
                contextNavigation(mission) {
                Text(mission.text).font(.footnote).foregroundStyle(.secondary).lineLimit(3)
                }
            }
            ForEach(card.context.filter { ![Icon.node, .task, .mission].contains($0.icon) }, id: \.text) { line in
                contextNavigation(line) {
                Label(line.text, systemImage: contextSymbol(line.icon))
                    .font(.subheadline).foregroundStyle(.secondary).lineLimit(2)
                }
            }
            if let body = card.body {
                if card.icon == .delivery || card.icon == .document {
                    ArtifactLinks(text: body.text, tint: tint)
                }
                if card.icon == .warning {
                    command(body)
                } else if body.truncated || body.more != nil || body.text.count > 600 {
                    excerpt(body)
                } else {
                    MarkdownView(markdown: body).foregroundStyle(Palette.ink.opacity(0.9))
                }
            }
            ForEach(card.attachments, id: \.id) { attachment in
                Button { openCardDetail() } label: {
                    HStack(spacing: 12) {
                        Image(systemName: "doc.text.magnifyingglass").font(.title3).foregroundStyle(tint)
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Review the attachment").font(.headline).foregroundStyle(Palette.ink)
                            Text(attachment.name).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                        }
                        Spacer()
                        Image(systemName: "chevron.right").font(.footnote.weight(.bold)).foregroundStyle(.tertiary)
                    }
                    .padding(14)
                    .background(tint.opacity(0.10), in: .rect(cornerRadius: 18, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous).strokeBorder(tint.opacity(0.25)))
                }
                .buttonStyle(PressStyle())
                .matchedTransitionSource(id: card.id, in: zoom)
            }
            ForEach(card.quotes, id: \.label) { quote in
                VStack(alignment: .leading, spacing: 6) {
                    Text(quote.label).font(.subheadline.weight(.medium)).foregroundStyle(.secondary)
                    Text(quote.text).font(.callout).foregroundStyle(Palette.ink)
                }
                .padding(14).frame(maxWidth: .infinity, alignment: .leading)
                .background(Palette.hairline.opacity(0.5), in: .rect(cornerRadius: 14))
            }
            if let facts = card.facts { FactsView(facts: facts, onLink: handleLink) }
            if let checks = card.checks { ChecksView(checks: checks, onLink: handleLink) }
            ForEach(card.children, id: \.id) { child in
                VStack(alignment: .leading, spacing: 6) {
                    Text(child.title).font(.headline)
                    if let body = child.body { MarkdownView(markdown: body) }
                    ForEach(child.actions, id: \.label) { action in actionControl(action) }
                }
                .padding(14).frame(maxWidth: .infinity, alignment: .leading)
                .background(Palette.cardRaised.opacity(0.6), in: .rect(cornerRadius: 20, style: .continuous))
            }
            if let error = store.actions.error { Text(error).font(.caption).foregroundStyle(Palette.rose) }
        }
        .padding(.horizontal, 22).padding(.top, 22).padding(.bottom, 16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            Color.clear.contentShape(.rect)
                .onTapGesture { openCardDetail() }
        }
        .accessibilityAction(named: "Open") { openCardDetail() }
    }

    /// What a permission would run, as 8ccd131 showed it: the command itself, a few lines tall;
    /// a longer one fades out and opens whole in the card.
    private func command(_ markdown: Markdown) -> some View {
        let long = markdown.text.split(separator: "\n", omittingEmptySubsequences: false).count > Self.commandLines + 2
        return Button { openCardDetail() } label: {
            MarkdownView(markdown: markdown).foregroundStyle(Palette.ink.opacity(0.9))
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, maxHeight: long ? 150 : nil, alignment: .topLeading)
                .clipped()
                .mask(LinearGradient(stops: [.init(color: .black, location: long ? 0.6 : 1), .init(color: long ? .clear : .black, location: 1)],
                                     startPoint: .top, endPoint: .bottom))
                .contentShape(.rect)
        }
        .buttonStyle(PressStyle())
        .accessibilityHint(Text("Shows the whole command"))
    }

    private static let commandLines = 6

    private func excerpt(_ markdown: Markdown) -> some View {
        let blocks = MarkdownParser.blocks(from: markdown.text)
        let segments = MarkdownParser.segments(blocks.prefix(5))
        return Button { openCardDetail() } label: {
            VStack(alignment: .leading, spacing: 0) {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(segments) { segment in
                        MarkdownSegmentView(segment: segment)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                .fixedSize(horizontal: false, vertical: true)
                .font(.callout).foregroundStyle(Palette.ink.opacity(0.9))
                .padding(16).frame(maxWidth: .infinity, maxHeight: 132, alignment: .topLeading)
                .clipped()
                .mask(LinearGradient(stops: [.init(color: .black, location: 0.45), .init(color: .clear, location: 0.9)], startPoint: .top, endPoint: .bottom))
                HStack(spacing: 10) {
                    Image(systemName: "text.viewfinder").font(.title3)
                    VStack(alignment: .leading, spacing: 1) {
                        Text("Read it all").font(.headline).foregroundStyle(Palette.ink)
                        Text("\(blocks.count) blocks · tap one to comment on it")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Image(systemName: "chevron.right").font(.footnote.weight(.bold)).foregroundStyle(.tertiary)
                }
                .foregroundStyle(tint).padding(.horizontal, 16).padding(.vertical, 12)
                .background(tint.opacity(0.10))
                .overlay(alignment: .top) { Rectangle().fill(tint.opacity(0.25)).frame(height: 1) }
            }
            .background(Palette.cardRaised.opacity(0.6))
            .clipShape(.rect(cornerRadius: 20, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous).strokeBorder(tint.opacity(0.25)))
            .contentShape(.rect(cornerRadius: 20))
        }
        .buttonStyle(PressStyle())
        .matchedTransitionSource(id: card.id, in: zoom)
    }

    private func handleLink(_ link: AlondraContract.Link) {
        switch link {
        case let .view(key):
            store.send(.opened(ViewKey(rawValue: key)))
        case let .url(url):
            if let url = URL(string: url) { openURL(url) }
        case .local: break
        }
    }

    private func openCardDetail() {
        store.send(.opened(card.icon == .pullRequest ? .pullRequest(card.id) : .card(card.id)))
    }

    @ViewBuilder private func contextNavigation<Content: View>(
        _ line: ContextLine, @ViewBuilder content: () -> Content
    ) -> some View {
        if let link = line.link {
            Button { handleLink(link) } label: { content() }
                .buttonStyle(.plain)
                .accessibilityAddTraits(.isButton)
        } else {
            content()
        }
    }

    private var controls: some View {
        VStack(spacing: 8) {
            if isQuestion || (card.icon == .document && !card.actions.contains { verb($0) == "accept" }) {
                ForEach(card.actions.filter { $0.presentation == .options }, id: \.label) { action in
                    actionControl(action)
                }
            }
            if let replyAction {
                AttachmentStrip(store: store.scope(state: \.attachments, action: \.attachments))
                composer(placeholder: inputPlaceholder(replyAction), action: replyAction)
            }
            ForEach(controlActions, id: \.label) { action in actionControl(action) }
        }
    }

    @ViewBuilder private func actionControl(_ action: AlondraContract.Action) -> some View {
        if action.presentation == .options, !action.options.isEmpty {
            let compact = action.options.count == 2 && action.options.allSatisfy { $0.label.count <= 16 } && !typeSize.isAccessibilitySize
            let grid = compact ? AnyLayout(HStackLayout(spacing: 10)) : AnyLayout(VStackLayout(spacing: 8))
            grid {
                ForEach(action.options, id: \.id) { option in
                    Button {
                        store.send(.optionPicked(action, optionID: option.id))
                        if case .text = option.input { replyFocused = true }
                    } label: {
                        VStack(alignment: compact ? .center : .leading, spacing: 4) {
                            Text(option.label).font(.headline)
                            if !compact, let description = option.description {
                                Text(description).font(.caption).foregroundStyle(.secondary)
                            }
                        }
                            .frame(maxWidth: .infinity, minHeight: 46, alignment: compact ? .center : .leading)
                            .padding(.horizontal, compact ? 0 : 6)
                    }
                    .buttonStyle(OptionPillStyle(tint: tint, picked: store.actions.selectedOptionID == option.id))
                    .disabled(!action.enabled || store.actions.inFlight)
                }
            }
        } else { actionButton(action) }
    }

    @ViewBuilder private func actionButton(_ action: AlondraContract.Action) -> some View {
        if action.presentation == .menu, !action.options.isEmpty {
            Menu {
                ForEach(action.options, id: \.id) { option in
                    Button(option.label) {
                        store.send(.menuOptionPicked(action, optionID: option.id))
                    }
                }
            } label: {
                Label(action.label + "…", systemImage: "arrow.triangle.merge")
                    .font(.headline).frame(maxWidth: .infinity, minHeight: 50)
            }
            .buttonStyle(FilledStyle(tint: actionTint))
            .disabled(!action.enabled || store.actions.inFlight)
        } else {
            Button {
            if case .url(let url) = action.link {
                if let destination = URL(string: url) { openURL(destination) }
                return
            }
            store.send(.actionTapped(action))
            } label: {
                let prominent = isDocumentReview ? verb(action) == "changes" : action.style == .primary
                if verb(action) == "ctx" {
                    Label(action.label, systemImage: "questionmark.bubble")
                        .font(.subheadline.weight(.medium)).frame(minHeight: 30)
                } else if CardActionPresentation.isRescueComments(card: card, action: action) {
                    Label("Rescue Comments", systemImage: "text.bubble")
                        .font(.headline).frame(maxWidth: .infinity, minHeight: 50)
                } else if verb(action) == "ask_pr" {
                    Label(action.label, systemImage: "questionmark.bubble")
                        .font(.subheadline.weight(.medium)).frame(minHeight: 30)
                } else if verb(action) == "accept", isDocumentReview {
                    Label(action.label, systemImage: "checkmark.seal")
                        .font(.subheadline.weight(.medium)).frame(minHeight: 30)
                } else if CardActionPresentation.isOpenOnGitHub(action) {
                    Label(action.label, systemImage: "arrow.up.right.square")
                        .font(.subheadline.weight(.medium)).frame(maxWidth: .infinity, alignment: .leading)
                } else if verb(action) == "read" {
                    Label(action.label, systemImage: "checkmark")
                        .font(prominent ? .headline : .subheadline.weight(.medium))
                        .frame(maxWidth: .infinity, minHeight: prominent ? 50 : 30)
                } else {
                    Text(action.label).font(prominent ? .headline : .subheadline.weight(.medium))
                        .frame(maxWidth: .infinity, minHeight: prominent ? 50 : 30)
                }
            }
            .buttonStyle(AnyButtonStyle(tint: filledTint(for: action)))
            .disabled(!action.enabled || store.actions.inFlight ||
                (verb(action) == "ask_pr" && store.replyDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty))
        }
    }

    private func filledTint(for action: AlondraContract.Action) -> Color? {
        if isDocumentReview { return verb(action) == "changes" ? actionTint : nil }
        return action.style == .primary ? actionTint : nil
    }

    private func composer(placeholder: String, action: AlondraContract.Action) -> some View {
        HStack(alignment: .bottom, spacing: 8) {
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
                    .frame(width: 36, height: 36).background(tint, in: .circle)
            }
            .buttonStyle(PressStyle())
            .disabled((store.replyDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                && !(selected?.id == action.id && store.actions.selectedOptionID != nil)) || store.actions.inFlight)
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

    private func relativeStamp(_ value: String) -> String { relativeSince(value) ?? value }
    private func contextSymbol(_ icon: Icon) -> String {
        switch icon {
        case .question: "questionmark.bubble"
        case .action: "hand.point.right"
        case .document: "doc.richtext"
        case .pullRequest: "arrow.triangle.pull"
        case .node: "cpu"
        case .task: "checklist"
        case .mission: "scope"
        default: "circle.fill"
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

struct OptionPillStyle: ButtonStyle {
    let tint: Color
    let picked: Bool
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(picked ? Palette.onAccent : tint)
            .padding(.horizontal, 14).padding(.vertical, 4)
            .background(picked ? tint : tint.opacity(0.12), in: .rect(cornerRadius: 18, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous).strokeBorder(tint.opacity(picked ? 0 : 0.35)))
            .scaleEffect(configuration.isPressed ? 0.96 : 1)
            .animation(.spring(duration: 0.25, bounce: 0.4), value: configuration.isPressed)
    }
}

private struct AnyButtonStyle: ButtonStyle {
    let tint: Color?
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(tint == nil ? Color.secondary : Palette.onAccent)
            .background(tint ?? .clear, in: .rect(cornerRadius: 18, style: .continuous))
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
    }
}
