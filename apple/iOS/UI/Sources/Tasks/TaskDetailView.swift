import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// One task: its state and what it is doing now, what it waits for from you, its request, pull
/// requests, dependencies, documents, options and history, with relaunch, cancel, reopen and archive.
public struct TaskDetailView: View {
    @Bindable var store: StoreOf<TaskDetailFeature>

    public init(store: StoreOf<TaskDetailFeature>) { self.store = store }

    public var body: some View {
        Group {
            if let detail = store.detail {
                ScrollView {
                    VStack(alignment: .leading, spacing: 24) {
                        header(detail)
                        waiting(detail)
                        request(detail)
                        pullRequests(detail)
                        dependencies(detail)
                        documents(detail)
                        options(detail)
                        history(detail)
                        actions(detail)
                    }
                    .padding(.horizontal, 20)
                    .padding(.top, 4)
                    .padding(.bottom, 32)
                }
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
        .sensoryFeedback(.success, trigger: store.actions.stamp)
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
            if let node = detail.node {
                Button {
                    open(node.link)
                } label: {
                    Text(verbatim: node.text)
                        .font(.mono(.caption))
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
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
                        HStack(alignment: .firstTextBaseline) {
                            Text(verbatim: fact.label)
                            Spacer(minLength: 12)
                            Text(verbatim: fact.value)
                                .foregroundStyle(.secondary)
                                .multilineTextAlignment(.trailing)
                        }
                        .padding(.horizontal, 16)
                        .padding(.vertical, 13)
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
        let shown = [detail.relaunch, detail.cancel, detail.reopen, detail.archive].compactMap(\.self).filter { !store.optimistic.hides($0.id) }
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
        case "cancel": "xmark.circle"
        case "reopen": "arrow.clockwise"
        default: "archivebox"
        }
    }

    private static func tint(for action: WireAction) -> Color {
        switch action.command {
        case "relaunch": Palette.sage
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
            Text("\(Text(verbatim: step.state).fontWeight(.semibold))\(Text(verbatim: step.summary.map { " · \($0)" } ?? ""))")
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
