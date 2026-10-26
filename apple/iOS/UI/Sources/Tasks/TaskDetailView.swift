import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// One task: its state, folder and what its node reports, what the node is doing right now and
/// needs, how far its steps, subagents and workflows have got, what runs in the background and what
/// it said last, what it waits for from you, its request, pull requests, dependencies, documents,
/// options, the commands it lets through without asking and history, with interrupt, relaunch,
/// cancel and reopen and a composer to
/// write to its node.
public struct TaskDetailView: View {
    @Bindable var store: StoreOf<TaskDetailFeature>

    public init(store: StoreOf<TaskDetailFeature>) { self.store = store }

    public var body: some View {
        Group {
            if let detail = store.detail {
                ScrollView {
                    VStack(alignment: .leading, spacing: 24) {
                        header(detail)
                        activity(detail)
                        needs(detail)
                        waiting(detail)
                        progress(detail.steps, fallback: String(localized: "Steps"), symbol: "checklist")
                        progress(detail.subagents, fallback: String(localized: "Subagents"), symbol: "person.2")
                        progress(detail.workflows, fallback: String(localized: "Workflows"), symbol: "flowchart")
                        background(detail)
                        lastMessage(detail)
                        request(detail)
                        pullRequests(detail)
                        dependencies(detail)
                        documents(detail)
                        options(detail)
                        allowed(detail)
                        history(detail)
                        actions(detail)
                    }
                    .padding(.horizontal, 20)
                    .padding(.top, 4)
                    .padding(.bottom, 32)
                }
                .scrollDismissesKeyboard(.interactively)
                .safeAreaBar(edge: .bottom) { if detail.message != nil { composer(detail) } }
            } else {
                ScrollView {
                    ScreenPhaseView(phase: store.screen.phase, missing: "This task is gone") { store.send(.task) }
                }
            }
        }
        .background(DawnBackground(glow: TaskStyle.tint(tone: store.detail?.stateTone)))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .task { await store.send(.task).finish() }
        .actionDialogs(store.scope(state: \.actions, action: \.actions))
        .sensoryFeedback(.success, trigger: store.composer.stamp) { _, new in new != nil }
    }

    private func open(_ link: AlondraContract.Link?) {
        if let link { store.send(.open(link)) }
    }

    private func header(_ detail: TaskDetail) -> some View {
        let tint = TaskStyle.tint(tone: detail.stateTone)
        return VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                Glyph(symbol: TaskStyle.symbol(tone: detail.stateTone), tint: tint, pulse: detail.stateTone == .accent)
                if let state = detail.state {
                    Text(verbatim: state)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(tint)
                }
                if let since = detail.since {
                    HStack(spacing: 4) {
                        Text("since")
                        RelativeTime(date: since)
                    }
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                }
            }
            Text(verbatim: detail.title)
                .font(.display(.largeTitle, weight: .bold))
                .foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
            if let request = detail.request {
                Text("from “\(request.text)”")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.leading)
            }
            if let folder = detail.folder {
                Text(verbatim: folder.replacing(/^\/Users\/[^\/]+\//, with: "~/"))
                    .font(.mono(.caption))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            if !detail.readings.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(detail.readings.enumerated(), id: \.offset) { _, reading in
                        HStack(alignment: .firstTextBaseline, spacing: 6) {
                            Text(verbatim: reading.label).foregroundStyle(.secondary)
                            Text(verbatim: reading.value)
                                .foregroundStyle(reading.tone.map { TaskStyle.tint(tone: $0) } ?? Palette.ink)
                                .lineLimit(2)
                        }
                        .font(.caption)
                    }
                }
                .accessibilityElement(children: .combine)
            }
            if let now = detail.now {
                Label {
                    Text(verbatim: now.text)
                } icon: {
                    Image(systemName: "clock")
                }
                .font(.mono(.footnote))
                .foregroundStyle(Palette.sky)
                .lineLimit(3)
            }
        }
    }

    @ViewBuilder private func activity(_ detail: TaskDetail) -> some View {
        if !detail.activity.elements.isEmpty {
            DetailSection(title: detail.activity.title ?? String(localized: "Right now"), symbol: "wrench.and.screwdriver") {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(detail.activity.elements) { ActivityLine(activity: $0) }
                }
                .padding(16)
            }
        }
    }

    @ViewBuilder private func needs(_ detail: TaskDetail) -> some View {
        if let needs = detail.needs.elements.first {
            DetailSection(title: detail.needs.title ?? String(localized: "What it needs"), symbol: "hand.raised") {
                MarkdownView(markdown: needs).padding(16)
            }
        }
    }

    @ViewBuilder private func progress(_ section: TaskDetail.Section<TaskDetail.Progress>, fallback: String, symbol: String) -> some View {
        if !section.elements.isEmpty {
            DetailSection(title: section.title ?? fallback, symbol: symbol) {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(section.elements.enumerated(), id: \.offset) { _, item in ProgressLine(item: item) }
                }
                .padding(16)
            }
        }
    }

    @ViewBuilder private func background(_ detail: TaskDetail) -> some View {
        if !detail.background.elements.isEmpty {
            DetailSection(title: detail.background.title ?? String(localized: "Running in the background"), symbol: "square.stack.3d.up") {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(detail.background.elements) { BackgroundRow(activity: $0) }
                }
                .padding(16)
            }
        }
    }

    @ViewBuilder private func lastMessage(_ detail: TaskDetail) -> some View {
        if let message = detail.lastMessage {
            DetailSection(title: message.title ?? String(localized: "What it said last"), symbol: "text.bubble") {
                VStack(alignment: .leading, spacing: 8) {
                    MarkdownView(markdown: message.markdown)
                    if let at = message.at {
                        TimeText(raw: at).font(.caption).foregroundStyle(.tertiary)
                    }
                }
                .padding(16)
            }
        }
    }

    private func composer(_ detail: TaskDetail) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            AttachmentStrip(store: store.scope(state: \.composer.attachmentPicker, action: \.composer.attachmentPicker))
            HStack(alignment: .bottom, spacing: 10) {
                HStack(alignment: .bottom, spacing: 4) {
                    AttachButton(store: store.scope(state: \.composer.attachmentPicker, action: \.composer.attachmentPicker))
                        .padding(.leading, 6).padding(.bottom, 4)
                    TextField(composerPlaceholder(detail.message), text: Binding(get: { store.message }, set: { store.send(.messageChanged($0)) }),
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

    @ViewBuilder private func waiting(_ detail: TaskDetail) -> some View {
        if !detail.waiting.elements.isEmpty {
            DetailSection(title: detail.waiting.title ?? String(localized: "Waiting for You"), symbol: "tray.full") {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(detail.waiting.elements, id: \.id) { card in
                        Button {
                            open(card.link)
                        } label: {
                            HStack(spacing: 10) {
                                KindBadge(kind: Kind(card))
                                Text(verbatim: card.title)
                                    .font(.subheadline.weight(.medium))
                                    .foregroundStyle(Palette.ink)
                                    .lineLimit(2)
                                    .multilineTextAlignment(.leading)
                                Spacer(minLength: 0)
                                Image(systemName: "chevron.right")
                                    .font(.caption.weight(.bold))
                                    .foregroundStyle(.tertiary)
                            }
                            .contentShape(.rect)
                        }
                        .buttonStyle(PressStyle())
                    }
                }
                .padding(16)
            }
        }
    }

    @ViewBuilder private func request(_ detail: TaskDetail) -> some View {
        if let text = detail.requestText.elements.first {
            DetailSection(title: detail.requestText.title ?? String(localized: "Request"), symbol: "text.quote") {
                Text(verbatim: text)
                    .font(.body)
                    .foregroundStyle(Palette.ink)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                    .padding(16)
            }
        }
    }

    @ViewBuilder private func pullRequests(_ detail: TaskDetail) -> some View {
        if !detail.pullRequests.elements.isEmpty {
            DetailSection(title: detail.pullRequests.title ?? String(localized: "Pull requests"), symbol: "arrow.triangle.merge") {
                VStack(spacing: 0) {
                    ForEach(Array(detail.pullRequests.elements.enumerated()), id: \.element.id) { index, row in
                        Button {
                            open(row.link)
                        } label: {
                            PullRequestLine(row: row)
                        }
                        .buttonStyle(PressStyle())
                        .overlay(alignment: .top) { if index > 0 { Hairline() } }
                    }
                }
            }
        }
    }

    @ViewBuilder private func dependencies(_ detail: TaskDetail) -> some View {
        if !detail.dependencies.elements.isEmpty {
            DetailSection(title: detail.dependencies.title ?? String(localized: "Dependencies"), symbol: "link") {
                VStack(spacing: 0) {
                    ForEach(Array(detail.dependencies.elements.enumerated()), id: \.element.id) { index, row in
                        Button {
                            open(row.link)
                        } label: {
                            DependencyLine(row: row)
                        }
                        .buttonStyle(PressStyle())
                        .overlay(alignment: .top) { if index > 0 { Hairline() } }
                    }
                }
            }
        }
    }

    @ViewBuilder private func documents(_ detail: TaskDetail) -> some View {
        if !detail.documents.elements.isEmpty {
            DetailSection(title: detail.documents.title ?? String(localized: "Documents"), symbol: "doc.text") {
                VStack(spacing: 0) {
                    ForEach(Array(detail.documents.elements.enumerated()), id: \.element.id) { index, row in
                        Button {
                            open(row.link)
                        } label: {
                            HStack(spacing: 12) {
                                Image(systemName: "doc.text")
                                    .font(.title3)
                                    .foregroundStyle(Palette.sky)
                                    .frame(width: 22)
                                Text(verbatim: row.title)
                                    .font(.body.weight(.semibold))
                                    .foregroundStyle(Palette.ink)
                                    .multilineTextAlignment(.leading)
                                Spacer(minLength: 0)
                                Image(systemName: "chevron.right")
                                    .font(.caption.weight(.bold))
                                    .foregroundStyle(.tertiary)
                            }
                            .padding(.horizontal, 16)
                            .padding(.vertical, 13)
                            .contentShape(.rect)
                        }
                        .buttonStyle(PressStyle())
                        .overlay(alignment: .top) { if index > 0 { Hairline() } }
                    }
                }
            }
        }
    }

    @ViewBuilder private func options(_ detail: TaskDetail) -> some View {
        if !detail.options.elements.isEmpty {
            DetailSection(title: detail.options.title ?? String(localized: "Options"), symbol: "slider.horizontal.3") {
                VStack(spacing: 0) {
                    ForEach(Array(detail.options.elements.enumerated()), id: \.offset) { index, fact in
                        Group {
                            if let priority = detail.priority, fact.label == priority.label {
                                priorityMenu(priority, value: fact.value)
                            } else {
                                HStack(alignment: .firstTextBaseline) {
                                    Text(verbatim: fact.label)
                                    Spacer(minLength: 12)
                                    Text(verbatim: fact.value)
                                        .foregroundStyle(.secondary)
                                        .multilineTextAlignment(.trailing)
                                }
                            }
                        }
                        .padding(.horizontal, 16)
                        .padding(.vertical, 13)
                        .overlay(alignment: .top) { if index > 0 { Hairline() } }
                    }
                }
            }
        }
    }

    private func priorityMenu(_ priority: WireAction, value: String) -> some View {
        let shown = store.optimistic.hides(priority.id) ? store.choices[priority.id ?? ""] ?? value : value
        return Menu {
            ForEach(priority.options, id: \.id) { option in
                Button { store.send(.choose(priority, option)) } label: {
                    Text(option.label)
                    if let description = option.description { Text(description) }
                }
            }
        } label: {
            HStack(alignment: .firstTextBaseline) {
                Text(verbatim: priority.label).foregroundStyle(Palette.ink)
                Spacer(minLength: 12)
                Text(verbatim: shown).foregroundStyle(Palette.dawn)
                Image(systemName: "chevron.up.chevron.down").font(.caption).foregroundStyle(.secondary)
            }
            .contentShape(.rect)
        }
        .accessibilityIdentifier("task.priority")
    }

    /// A revoked grant leaves at once and comes back with the error if the daemon refuses.
    @ViewBuilder private func allowed(_ detail: TaskDetail) -> some View {
        let shown = detail.allowed.elements.filter { !store.optimistic.hides($0.revoke?.id) }
        if !shown.isEmpty {
            DetailSection(title: detail.allowed.title ?? String(localized: "Allowed in this task"), symbol: "checkmark.shield") {
                VStack(spacing: 0) {
                    ForEach(shown.enumerated(), id: \.element.id) { index, grant in
                        GrantLine(grant: grant) { revoke in store.send(.perform(revoke)) }
                            .overlay(alignment: .top) { if index > 0 { Hairline() } }
                    }
                }
            }
        }
    }

    @ViewBuilder private func history(_ detail: TaskDetail) -> some View {
        if !detail.history.elements.isEmpty {
            DetailSection(title: detail.history.title ?? String(localized: "History"), symbol: "clock.arrow.circlepath") {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(detail.history.elements.enumerated()), id: \.offset) { index, step in
                        HistoryLine(step: step, first: index == 0, last: index == detail.history.elements.count - 1)
                    }
                }
                .padding(.vertical, 6)
            }
        }
    }

    @ViewBuilder private func actions(_ detail: TaskDetail) -> some View {
        let shown = [detail.interrupt, detail.resume, detail.relaunch, detail.pause, detail.cancel, detail.reopen].compactMap(\.self).filter { !store.optimistic.hides($0.id) }
        if !shown.isEmpty {
            VStack(spacing: 10) {
                ForEach(shown, id: \.id) { action in
                    ActionButton(
                        title: action.label, symbol: Self.symbol(for: action), tint: Self.tint(for: action)
                    ) { store.send(.perform(action)) }
                }
            }
        }
    }

    private static func symbol(for action: WireAction) -> String {
        switch action.command {
        case "relaunch": "arrow.triangle.2.circlepath"
        case "interrupt", "force_stop": "stop.circle"
        case "resume": "play.circle"
        case "pause": "pause.circle"
        case "cancel": "xmark.circle"
        default: "arrow.clockwise"
        }
    }

    private static func tint(for action: WireAction) -> Color {
        switch action.command {
        case "relaunch": Palette.sage
        case "interrupt", "force_stop": Palette.ember
        case "resume": Palette.sky
        case "cancel": Palette.rose
        case "reopen": Palette.sky
        default: Palette.slate
        }
    }
}

private struct PullRequestLine: View {
    let row: Row

    private var order: String { row.title.split(separator: "·").first.map { $0.trimmingCharacters(in: .whitespaces) } ?? "" }
    private var title: String {
        guard let separator = row.title.range(of: " · ") else { return row.title }
        return String(row.title[separator.upperBound...])
    }

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Text(verbatim: order)
                .font(.caption.weight(.bold).monospacedDigit())
                .frame(width: 24, height: 24)
                .background(Palette.hairline, in: .circle)
            VStack(alignment: .leading, spacing: 4) {
                Text(verbatim: title)
                    .font(.body.weight(.semibold))
                    .foregroundStyle(Palette.ink)
                    .multilineTextAlignment(.leading)
                if let branches = row.subtitle {
                    Text(verbatim: branches)
                        .font(.mono(.caption))
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.leading)
                }
            }
            Spacer(minLength: 0)
            Image(systemName: "chevron.right")
                .font(.caption.weight(.bold))
                .foregroundStyle(.tertiary)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 13)
        .contentShape(.rect)
    }
}

private struct DependencyLine: View {
    let row: Row

    var body: some View {
        let tint = TaskStyle.tint(icon: row.icon, tone: row.badges.first?.tone)
        HStack(spacing: 10) {
            Glyph(symbol: TaskStyle.symbol(icon: row.icon, struck: row.struck), tint: tint, size: .small)
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: row.title)
                    .font(.body.weight(.medium))
                    .foregroundStyle(row.struck ? AnyShapeStyle(.secondary) : AnyShapeStyle(Palette.ink))
                    .strikethrough(row.struck)
                    .multilineTextAlignment(.leading)
                if let state = row.subtitle {
                    Text(verbatim: state)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(tint)
                }
            }
            Spacer(minLength: 0)
            Image(systemName: "chevron.right")
                .font(.caption.weight(.bold))
                .foregroundStyle(.tertiary)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 13)
        .contentShape(.rect)
    }
}

private struct HistoryLine: View {
    let step: TaskDetail.Step
    let first: Bool
    let last: Bool

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Group {
                if let at = step.at {
                    Text(at, format: .dateTime.hour().minute())
                } else {
                    Text(verbatim: "")
                }
            }
            .font(.mono(.caption))
            .foregroundStyle(.tertiary)
            .frame(width: 44, alignment: .leading)
            Glyph(symbol: "circle.fill", tint: Palette.slate, size: .tiny)
                .background(Palette.card, in: .circle)
            Text("\(Text(verbatim: step.state).fontWeight(.semibold))\(Text(step.summary.map { AttributedString(" · ") + MarkdownBlock.line($0) } ?? AttributedString()))")
                .font(.subheadline)
                .foregroundStyle(Palette.ink.opacity(0.86))
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 9)
        .background(alignment: .topLeading) {
            GeometryReader { proxy in
                Path { path in
                    let x: CGFloat = 16 + 44 + 10 + 9
                    path.move(to: CGPoint(x: x, y: first ? 18 : 0))
                    path.addLine(to: CGPoint(x: x, y: last ? 18 : proxy.size.height))
                }
                .stroke(Palette.hairline, lineWidth: 2)
            }
        }
    }
}
