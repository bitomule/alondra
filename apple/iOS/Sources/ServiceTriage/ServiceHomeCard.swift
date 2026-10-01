import AlondraShared
import AlondraAttachmentsFeature
import AlondraViews
import AlondraViewsUI
import ComposableArchitecture
import SwiftUI

/// Home-specific presentation of a wire Card. It keeps the established 34 pt shell,
/// scrolling body, pinned composer, spring and stamp treatment.
struct ServiceHomeCard: View {
    let card: Card
    @Bindable var actions: StoreOf<ActionFeature>
    let attachments: StoreOf<AttachmentFeature>
    let isFront: Bool
    let zoom: Namespace.ID
    let submit: () -> Void
    let openReview: (String) -> Void
    @Environment(\.openURL) private var openURL
    @State private var contentHeight: CGFloat?
    @State private var replyDraft = ""
    @State private var contextAction: AlondraViews.Action?
    @State private var contextNote = ""
    @FocusState private var replyFocused: Bool
    @Environment(\.dynamicTypeSize) private var typeSize

    private var kind: Kind { Kind(card) }
    private var tint: Color { kind.tint }
    private var actionTint: Color { kind.tint == Palette.slate ? Palette.dawn : kind.tint }
    private var allActions: [AlondraViews.Action] {
        card.actions + card.children.flatMap(\.actions)
    }
    private var selected: AlondraViews.Action? {
        guard let action = actions.action, allActions.contains(where: { $0.id == action.id }) else { return nil }
        return action
    }
    private var replyAction: AlondraViews.Action? {
        let preferred: [String] = switch card.icon {
        case .pullRequest, .document: ["changes", "reply", "ans", "ask_pr"]
        default: ["ans", "reply", "changes", "ask_pr"]
        }
        for verb in preferred {
            if let action = allActions.first(where: { isReplyAction($0, verb: verb) }) { return action }
        }
        return nil
    }
    private func isReplyAction(_ action: AlondraViews.Action, verb: String) -> Bool {
        guard action.enabled, action.id?.hasPrefix(verb + ":") == true else { return false }
        if action.presentation == .options { return true }
        if case .text = action.input { return true }
        return false
    }
    private func inputPlaceholder(_ action: AlondraViews.Action) -> String {
        if card.icon == .pullRequest { return String(localized: "What should change?") }
        if case let .text(value, _, _) = action.input {
            return value ?? String(localized: "Write your answer")
        }
        return String(localized: "Write your answer")
    }
    private var stamp: String? { selected == nil ? nil : actions.stamp }
    private var isQuestion: Bool { card.icon == .question || card.icon == .action }
    private var isDocumentReview: Bool { card.icon == .document || card.icon == .delivery }
    private func verb(_ action: AlondraViews.Action) -> String {
        action.id?.split(separator: ":", maxSplits: 1).first.map(String.init) ?? ""
    }
    private func controlRank(_ action: AlondraViews.Action) -> Int {
        switch (card.icon, verb(action)) {
        case (.question, "ctx"), (.action, "ctx"): return 1
        case (.document, "changes"), (.delivery, "changes"): return 1
        case (.document, "accept"), (.delivery, "accept"): return 2
        case (.pullRequest, "ask_pr"): return 1
        case (.pullRequest, "changes"): return 2
        case (.pullRequest, "merge"): return 3
        default: return 4
        }
    }
    private var controlActions: [AlondraViews.Action] {
        card.actions.filter { action in
            action.presentation != .options && (action.id != replyAction?.id || action.comments != nil)
        }.sorted { controlRank($0) < controlRank($1) }
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
            .onChange(of: actions.stamp) { old, new in
                if old == nil, new != nil { replyDraft = "" }
            }
            .alert("What do you need to decide?", isPresented: Binding(
                get: { contextAction != nil }, set: { if !$0 { contextAction = nil } }
            )) {
                TextField("Optional", text: $contextNote)
                Button("Cancel", role: .cancel) { contextAction = nil }
                Button("Ask for Context") {
                    guard let contextAction else { return }
                    actions.send(.present(contextAction))
                    actions.send(.textChanged(contextNote))
                    submit()
                    self.contextAction = nil
                }
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
                        .font(.caption2.weight(.bold)).foregroundStyle(tint)
                        .frame(width: 22, height: 22)
                        .background(tint.opacity(0.15), in: .circle)
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
                    ServiceArtifactLinks(text: body.text, tint: tint)
                }
                if body.truncated || body.more != nil || body.text.count > 600 {
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
            if let error = actions.error { Text(error).font(.caption).foregroundStyle(Palette.rose) }
        }
        .padding(.horizontal, 22).padding(.top, 22).padding(.bottom, 16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            Color.clear.contentShape(.rect)
                .onTapGesture { openCardDetail() }
        }
        .accessibilityAction(named: "Open") { openCardDetail() }
    }

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

    private func handleLink(_ link: AlondraViews.Link) {
        switch link {
        case let .view(key):
            openReview(key)
        case let .url(url):
            if let url = URL(string: url) { openURL(url) }
        case .local: break
        }
    }

    private func openCardDetail() {
        openReview(card.icon == .pullRequest ? "pull_request:\(card.id)" : "card:\(card.id)")
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
            if isQuestion {
                ForEach(card.actions.filter { $0.presentation == .options }, id: \.label) { action in
                    actionControl(action)
                }
            }
            if let replyAction {
                ServiceAttachmentStrip(store: attachments)
                composer(placeholder: inputPlaceholder(replyAction), action: replyAction)
            }
            ForEach(controlActions, id: \.label) { action in actionControl(action) }
        }
    }

    @ViewBuilder private func actionControl(_ action: AlondraViews.Action) -> some View {
        if action.presentation == .options, !action.options.isEmpty {
            let compact = action.options.count == 2 && action.options.allSatisfy { $0.label.count <= 16 } && !typeSize.isAccessibilitySize
            let grid = compact ? AnyLayout(HStackLayout(spacing: 10)) : AnyLayout(VStackLayout(spacing: 8))
            grid {
                ForEach(action.options, id: \.id) { option in
                    Button {
                        if selected?.id != action.id { actions.send(.present(action)) }
                        actions.send(.chooseOption(option.id))
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
                    .buttonStyle(HomeOptionStyle(tint: tint, picked: actions.selectedOptionID == option.id))
                    .disabled(!action.enabled || actions.inFlight)
                }
            }
        } else { actionButton(action) }
    }

    @ViewBuilder private func actionButton(_ action: AlondraViews.Action) -> some View {
        if action.presentation == .menu, !action.options.isEmpty {
            Menu {
                ForEach(action.options, id: \.id) { option in
                    Button(option.label) {
                        actions.send(.present(action))
                        actions.send(.chooseOption(option.id))
                        submit()
                    }
                }
            } label: {
                Label(action.label + "…", systemImage: "arrow.triangle.merge")
                    .font(.headline).frame(maxWidth: .infinity, minHeight: 50)
            }
            .buttonStyle(ServiceFilledStyle(tint: actionTint))
            .disabled(!action.enabled || actions.inFlight)
        } else {
            Button {
            if verb(action) == "ctx" {
                contextNote = ""
                contextAction = action
                return
            }
            if verb(action) == "ask_pr" {
                actions.send(.present(action))
                actions.send(.textChanged(replyDraft))
                submit()
                return
            }
            if action.comments != nil {
                if case let .view(key) = action.link { openReview(key) }
                else { openReview("card:\(card.id)") }
                return
            }
            if let link = action.link {
                switch link {
                case let .view(key):
                    openReview(key)
                    return
                case let .url(url):
                    if let url = URL(string: url) { openURL(url) }
                    return
                case .local:
                    return
                }
            }
            actions.send(.present(action))
            if case .none = action.input, action.presentation == .button { submit() }
            } label: {
                let prominent = isDocumentReview ? verb(action) == "changes" : action.style == .primary
                if verb(action) == "ctx" {
                    Label(action.label, systemImage: "questionmark.bubble")
                        .font(.subheadline.weight(.medium)).frame(minHeight: 30)
                } else if verb(action) == "read" {
                    Label(action.label, systemImage: "checkmark")
                        .font(prominent ? .headline : .subheadline.weight(.medium))
                        .frame(maxWidth: .infinity, minHeight: prominent ? 50 : 30)
                } else {
                    Text(action.label).font(prominent ? .headline : .subheadline.weight(.medium))
                        .frame(maxWidth: .infinity, minHeight: prominent ? 50 : 30)
                }
            }
            .buttonStyle(AnyButtonStyle(tint: (isDocumentReview ? verb(action) == "changes" : action.style == .primary) ? actionTint : nil))
            .disabled(!action.enabled || actions.inFlight ||
                (verb(action) == "ask_pr" && replyDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty))
        }
    }

    private func composer(placeholder: String, action: AlondraViews.Action) -> some View {
        HStack(alignment: .bottom, spacing: 8) {
            if action.attachments {
                ServiceAttachButton(store: attachments).padding(.leading, 6).padding(.bottom, 4)
            }
            TextField(placeholder, text: $replyDraft, axis: .vertical)
                .lineLimit(1...5).focused($replyFocused)
                .padding(.leading, 16).padding(.vertical, 12)
                .onChange(of: replyDraft) { _, new in
                    guard !new.isEmpty, selected?.id == action.id,
                          let optionID = actions.selectedOptionID,
                          let option = action.options.first(where: { $0.id == optionID }) else { return }
                    if case .none = option.input { actions.send(.present(action)) }
                }
            Button {
                replyFocused = false
                if selected?.id != action.id {
                    actions.send(.present(action))
                }
                if !replyDraft.isEmpty { actions.send(.textChanged(replyDraft)) }
                submit()
            } label: {
                Image(systemName: "arrow.up").font(.body.weight(.bold)).foregroundStyle(Palette.onAccent)
                    .frame(width: 36, height: 36).background(tint, in: .circle)
            }
            .buttonStyle(ServicePressStyle())
            .disabled((replyDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                && !(selected?.id == action.id && actions.selectedOptionID != nil)) || actions.inFlight)
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

    private func relativeStamp(_ value: String) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let date = formatter.date(from: value) ?? ISO8601DateFormatter().date(from: value)
        guard let date else { return value }
        return relative(Int64(date.timeIntervalSince1970 * 1_000))
    }
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

struct ServiceArtifactLink: View {
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

struct ServiceArtifactLinks: View {
    let text: String
    let tint: Color

    var body: some View {
        ForEach(Artifacts.urls(in: text), id: \.self) { url in
            ServiceArtifactLink(url: url, tint: tint).padding(.bottom, 12)
        }
    }
}

private struct HomeOptionStyle: ButtonStyle {
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
