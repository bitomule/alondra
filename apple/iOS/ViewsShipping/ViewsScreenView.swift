import AlondraContract
import AlondraFeed
import AlondraFeatures
import AlondraDesign
import AlondraiOSUI
import ComposableArchitecture
import SwiftUI
import UIKit

struct ViewsScreenView: View {
    @Bindable var store: StoreOf<ViewsScreenFeature>
    let openView: (ViewKey) -> Void
    @Environment(\.openURL) private var openURL
    @Environment(\.mcpLinks) private var mcpLinks
    @State private var diffComments: [String: ActionComment] = [:]

    private var isTasksScreen: Bool { store.key.rawValue == "tasks" || store.key.rawValue.hasPrefix("tasks?") }
    private var isInlineForm: Bool { store.hasInlineForm }
    private var pickerRows: [AlondraContract.Row] {
        store.pickerDocument?.sections.flatMap(\.items).compactMap { item in
            if case let .row(row) = item { row } else { nil }
        } ?? []
    }
    private var screenGlow: Color { Palette.dawn }

    var body: some View { alertsContent }

    private var lifecycleContent: some View {
        screenContent
        .task { await store.send(.appeared).finish() }
        .task(id: store.document?.version) {
            guard let document = store.document else { return }
            if store.key.rawValue.hasPrefix("pull_request:"),
               let diff = document.sections.lazy.flatMap(\.items).compactMap({ item -> ContentRef? in
                    if case let .contentRef(reference) = item, reference.kind == .diff { reference } else { nil }
                }).first {
                if store.contentReference?.digest != diff.digest {
                    diffComments = [:]
                    store.send(.fetchContent(diff))
                }
                return
            }
        }
        .onDisappear { store.send(.disappeared) }
    }

    private var actionSheetContent: some View {
        lifecycleContent
        .sheet(isPresented: Binding(get: { store.actionSheetPresented && !isInlineForm }, set: { if !$0 && store.actionSheetPresented && !isInlineForm { store.send(.action(.dismiss)) } })) {
            let action = store.scope(state: \.action, action: \.action)
            ActionComposerSheet(store: action, title: action.action?.label ?? "", submit: composerSubmit,
                                tint: screenGlow) {
                ActionInputView(store: action, form: store.form,
                                showCommentEditor: !store.key.rawValue.hasPrefix("pull_request:"),
                                onReload: { field, value in store.send(.reloadForm(field.id, value)) },
                                onPicker: { field, source in store.send(.pickerOpened(field.id, source)) })
            }
        }
    }

    private var composerSubmit: ActionComposerSheet<ActionInputView>.Submit {
        if store.form?.id == "schedule" { return .confirm(store.form?.submit.label ?? String(localized: "Save Schedule")) }
        if case .text = store.action.action?.input { return .send }
        return .confirm(String(localized: "Continue"))
    }

    private var pickerContent: some View {
        actionSheetContent
        .sheet(isPresented: Binding(get: { store.pickerFieldID != nil },
                                    set: { if !$0 { store.send(.pickerClosed) } })) { issuePickerPanel }
    }

    private var issuePickerPanel: some View {
        NavigationStack {
            List {
                if let error = store.pickerError {
                    Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.secondary)
                }
                ForEach(pickerRows, id: \.id) { row in
                    Button { store.send(.pickerSelected(row.id)) } label: {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(row.title).foregroundStyle(.primary)
                            if let subtitle = row.subtitle { Text(subtitle).font(.caption).foregroundStyle(.secondary) }
                        }
                    }
                }
                if pickerRows.isEmpty, let empty = store.pickerDocument?.empty {
                    Text(empty.text).foregroundStyle(.secondary)
                }
            }
            .overlay { if store.pickerLoading && pickerRows.isEmpty { ProgressView() } }
            .searchable(text: Binding(get: { store.pickerQuery }, set: { store.send(.pickerQueryChanged($0)) }),
                        prompt: Text("Filter issues"))
            .navigationTitle(store.pickerDocument?.title ?? String(localized: "Issues"))
            .toolbar { ToolbarItem(placement: .cancellationAction) {
                Button("Cancel") { store.send(.pickerClosed) }
            } }
        }
    }

    private var alertsContent: some View {
        pickerContent
        .actionDialogs(store.scope(state: \.action, action: \.action))
    }

    private var screenContent: some View {
        Group {
            if let document = store.document {
                documentScreen(document)
            } else if store.loading {
                ProgressView().tint(Palette.dawn).frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(DawnBackground(glow: screenGlow))
            } else {
                ContentUnavailableView(store.error ?? String(localized: "Unable to load this screen"), systemImage: "exclamationmark.triangle")
                    .background(DawnBackground(glow: screenGlow))
            }
        }
    }

    private func documentScreen(_ document: ViewDocument) -> some View {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 16) {
                        if isTasksScreen {
                            Text(document.title ?? document.status.summary)
                                .font(.display(.largeTitle, weight: .bold))
                                .foregroundStyle(Palette.ink)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.horizontal, 4)
                                .padding(.top, 4)
                        }
                        ForEach(document.notices, id: \.text) { notice in NoticeView(notice: notice) }
                        ForEach(document.sections, id: \.id) { section in
                            sectionContent(section)
                        }
                        if let content = store.content {
                            if store.contentReference?.kind == .diff {
                                DiffView(text: content, comments: $diffComments)
                                    .id(store.contentReference?.digest)
                            } else {
                                Text(content).font(.system(.body, design: .monospaced)).textSelection(.enabled)
                                    .padding(12).frame(maxWidth: .infinity, alignment: .leading)
                                    .background(.quaternary.opacity(0.35), in: .rect(cornerRadius: 12))
                            }
                        }
                        if let empty = document.empty, store.key.rawValue != "schedules" { NoticeView(notice: empty) }
                        if store.loading { ProgressView().frame(maxWidth: .infinity).padding() }
                        if let error = store.error {
                            ContentUnavailableView(error, systemImage: "exclamationmark.triangle")
                                .font(.callout)
                        }
                    }
                    .padding(16)
                }
                .background(DawnBackground(glow: screenGlow))
                .navigationTitle(isTasksScreen ? "" : (document.title ?? document.status.summary))
                .navigationBarTitleDisplayMode(isTasksScreen ? .inline : .automatic)
                .refreshable { await store.send(.refresh).finish() }
                .modifier(BottomBarModifier(enabled: hasBottomBar) { documentBottomBar })
                .toolbar(hidesTabBar ? .hidden : .automatic, for: .tabBar)
                .toolbar {
                    if store.key.rawValue == "schedules" {
                        ToolbarItem(placement: .topBarTrailing) {
                            Button("Add Schedule", systemImage: "plus") { open(.newSchedule()) }
                        }
                    }
                    ToolbarItem(placement: .topBarTrailing) {
                        Button("Refresh", systemImage: "arrow.clockwise") { store.send(.refresh) }
                    }
                }
    }

    private var hidesTabBar: Bool {
        store.key.rawValue.hasPrefix("task:")
    }

    private var hasBottomBar: Bool { isInlineForm && store.form != nil }

    @ViewBuilder private var documentBottomBar: some View {
        if isInlineForm, let form = store.form {
            Button(form.submit.label) { store.send(.action(.submit)) }
                .buttonStyle(.borderedProminent).tint(Palette.dawn)
                .disabled(store.action.inFlight || !form.submit.enabled)
                .frame(maxWidth: .infinity).padding(.horizontal, 20).padding(.vertical, 8)
        }
    }

    private func follow(_ link: AlondraContract.Link) {
        switch link {
        case .view(let key): open(ViewKey(rawValue: key))
        case .url(let raw):
            guard let url = URL(string: raw), url.scheme?.lowercased() == "https",
                  let host = url.host?.lowercased(), host == "github.com" || host.hasSuffix(".github.com") else { return }
            openURL(url)
        case .local(let local):
            if case let .attach(session) = local { open(.card(session)) } else { mcpLinks(link) }
        }
    }

    private func open(_ key: ViewKey) {
        openView(key)
    }

    private func present(_ action: AlondraContract.Action) {
        store.send(.action(.present(action)))
        if action.comments?.kind == .diffLines {
            store.send(.action(.commentsChanged(diffComments.values.sorted { $0.anchor < $1.anchor })))
        }
    }

    @ViewBuilder private func sectionContent(_ section: ViewSection) -> some View {
        if isInlineForm, section.id == "form", let form = store.form {
            ActionInputView(store: store.scope(state: \.action, action: \.action), form: form,
                            onReload: { field, value in store.send(.reloadForm(field.id, value)) },
                            onPicker: { field, source in store.send(.pickerOpened(field.id, source)) })
                .padding(4)
        } else if store.key.rawValue.hasPrefix("task:"), section.id == "header" {
            taskHeader(section)
        } else if store.key.rawValue.hasPrefix("task:"), section.id == "now" {
            taskNow(section)
        } else if store.key.rawValue.hasPrefix("task:"), section.id == "waiting" {
            taskWaiting(section)
        } else if isTasksScreen, ["active", "finished"].contains(section.id) {
            if !section.collapsed {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(alignment: .firstTextBaseline) {
                        if let title = section.title {
                            Text(title).font(.display(.title3, weight: .bold)).foregroundStyle(Palette.ink)
                        }
                        Spacer()
                        if let count = section.count, count > 0 { Text("\(count)").font(.caption.monospacedDigit()).foregroundStyle(.secondary) }
                    }
                    let rows = section.items.compactMap { item -> AlondraContract.Row? in
                        if case let .row(row) = item { row } else { nil }
                    }
                    if !rows.isEmpty {
                        VStack(spacing: 0) {
                            ForEach(rows, id: \.id) { row in
                                TaskRequestRow(row: row, onLink: follow)
                            }
                        }
                        .background(Palette.card.opacity(0.85), in: .rect(cornerRadius: 24, style: .continuous))
                    }
                    if let more = section.more {
                        Button("Show \(more.remaining) more") { open(ViewKey(rawValue: more.key)) }
                            .font(.footnote.weight(.semibold)).foregroundStyle(Palette.dawn)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.top, 24)
            }
        } else if store.key.rawValue == "schedules", section.id == "actions" {
            EmptyView()
        } else if store.key.rawValue == "schedules", section.id == "schedules", section.items.isEmpty,
                  let empty = store.document?.empty {
            ContentUnavailableView(empty.text, systemImage: "calendar.badge.clock",
                                   description: Text("Run a task automatically on a schedule."))
                .frame(maxWidth: .infinity, minHeight: 220)
        } else {
            SectionView(section: section, onAction: present,
                        onLink: follow, onMore: { open(ViewKey(rawValue: $0)) },
                        onForm: presentForm, onContent: { store.send(.fetchContent($0)) },
                        excludingActionIDs: excludedActionIDs(for: section))
        }
    }

    private func taskHeader(_ section: ViewSection) -> some View {
        let facts = section.items.compactMap { item -> [Fact]? in
            if case let .facts(value) = item { value.items } else { nil }
        }.flatMap { $0 }
        let state = facts.first?.value ?? ""
        let since = store.document?.sections.first(where: { $0.id == "history" })?.items.last.flatMap { item -> String? in
            guard case let .row(row) = item, let trailing = row.trailing,
                  case let .time(at, _) = trailing else { return nil }
            return relativeSince(at)
        }
        return VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                Glyph(symbol: "circle.dashed", tint: Palette.dawn, size: .small)
                Text(state.prefix(1).uppercased() + state.dropFirst())
                    .font(.subheadline.weight(.semibold)).foregroundStyle(Palette.dawn)
                if let since { Text("since \(since)").font(.subheadline).foregroundStyle(.secondary) }
            }
            Text(store.document?.title ?? "")
                .font(.display(.largeTitle, weight: .bold)).foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
            ForEach(Array(facts.indices.dropFirst()), id: \.self) { index in
                let fact = facts[index]
                Button { if let link = fact.link { follow(link) } } label: {
                    Text("from “\(fact.value)”").font(.subheadline).foregroundStyle(.secondary)
                }.buttonStyle(.plain)
            }
        }
    }

    private func taskNow(_ section: ViewSection) -> some View {
        let row = section.items.compactMap { item -> AlondraContract.Row? in
            if case let .row(row) = item { row } else { nil }
        }.first
        return Group {
            if let row {
                Button { if let link = row.link { follow(link) } } label: {
                    Label(row.title, systemImage: "clock")
                        .font(.mono(.footnote)).foregroundStyle(Palette.sky)
                        .lineLimit(3).multilineTextAlignment(.leading)
                }.buttonStyle(.plain)
            }
        }
    }

    private func taskWaiting(_ section: ViewSection) -> some View {
        let cards = section.items.compactMap { item -> AlondraContract.Card? in
            if case let .card(card) = item { card } else { nil }
        }
        return VStack(alignment: .leading, spacing: 10) {
            Label(section.title ?? String(localized: "Waiting for You"), systemImage: "tray.full")
                .font(.footnote.weight(.bold)).textCase(.uppercase).foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 8) {
                ForEach(cards, id: \.id) { card in
                    Button { if let link = card.link { follow(link) } } label: {
                        HStack(spacing: 10) {
                            KindBadge(kind: Kind(card))
                            Text(card.title).font(.subheadline.weight(.medium)).foregroundStyle(Palette.ink)
                                .lineLimit(2).multilineTextAlignment(.leading)
                            Spacer(minLength: 0)
                            Image(systemName: "chevron.right").font(.caption.weight(.bold)).foregroundStyle(.tertiary)
                        }
                    }.buttonStyle(.plain)
                }
            }
            .padding(16).frame(maxWidth: .infinity, alignment: .leading)
            .background(Palette.card.opacity(0.9), in: .rect(cornerRadius: 20))
        }
    }

    private func presentForm(_ form: WireForm) {
        store.send(.presentForm(form))
    }

    private func excludedActionIDs(for section: ViewSection) -> Set<String> {
        guard let document = store.document else { return [] }
        guard store.key.rawValue.hasPrefix("pull_request:"), section.id != "actions" else { return [] }
        return Set(document.sections.flatMap(\.items).flatMap { item -> [AlondraContract.Action] in
            if case let .actions(bar) = item { bar.actions } else { [] }
        }.compactMap(\.id))
    }
}

private struct TaskRequestRow: View {
    let row: AlondraContract.Row
    let onLink: (AlondraContract.Link) -> Void

    var body: some View {
        VStack(spacing: 0) {
            Button { if let link = row.link { onLink(link) } } label: {
                HStack(alignment: .top, spacing: 10) {
                    VStack(alignment: .leading, spacing: 5) {
                        if !row.badges.isEmpty {
                            ElapsedText(row.badges)
                                .font(.caption.weight(.medium)).foregroundStyle(.secondary).lineLimit(1)
                        }
                        Text(row.title).font(.display(.title3, weight: .bold)).foregroundStyle(Palette.ink)
                            .multilineTextAlignment(.leading).fixedSize(horizontal: false, vertical: true)
                        if let subtitle = row.subtitle { Text(subtitle).font(.caption).foregroundStyle(.secondary) }
                    }
                    Spacer(minLength: 0)
                    if case let .time(at, format) = row.trailing { ViewsDateText(at, format: format).font(.caption).foregroundStyle(.tertiary) }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 16).padding(.top, 16).padding(.bottom, row.children.isEmpty ? 16 : 8)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            ForEach(row.children, id: \.id) { child in TaskWireRow(row: child, onLink: onLink) }
        }
        .overlay(alignment: .bottom) { Palette.hairline.frame(height: 1).padding(.horizontal, 14) }
    }
}

private struct TaskWireRow: View {
    let row: AlondraContract.Row
    let onLink: (AlondraContract.Link) -> Void
    private var tint: Color {
        switch row.icon {
        case .mission: return .green
        case .action: return .pink
        case .pullRequest: return .mint
        case .branch: return .indigo
        case .check: return row.struck ? Palette.slate : .teal
        case .warning: return Palette.slate
        default: break
        }
        guard let tone = row.badges.first?.tone else { return Palette.slate }
        return switch tone {
        case .neutral: Palette.slate
        case .info: Palette.sky
        case .accent: Palette.dawn
        case .success: Palette.sage
        case .warning: Palette.dawn
        case .critical: Palette.rose
        case .yourTurn: Palette.ember
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            Button { if let link = row.link { onLink(link) } } label: {
                HStack(alignment: .top, spacing: 10) {
                    Glyph(symbol: symbol, tint: tint, size: .small)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(row.title).font(.subheadline.weight(.semibold)).foregroundStyle(row.struck ? .secondary : Palette.ink)
                            .strikethrough(row.struck).multilineTextAlignment(.leading)
                        if let subtitle = row.subtitle { Text(subtitle).font(.caption.weight(.semibold)).foregroundStyle(tint) }
                    }
                    Spacer(minLength: 0)
                    if case let .count(value) = row.trailing, value > 0 {
                        Text(value, format: .number).font(.caption.weight(.bold).monospacedDigit())
                            .foregroundStyle(Palette.onAccent).frame(minWidth: 22, minHeight: 22).background(Palette.dawn, in: .capsule)
                    }
                    if row.link != nil { Image(systemName: "chevron.right").font(.caption.weight(.bold)).foregroundStyle(.tertiary) }
                }
                .padding(.leading, 16 + CGFloat(row.depth) * 24).padding(.trailing, 14).padding(.vertical, 12)
                .frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            ForEach(row.children, id: \.id) { child in TaskWireRow(row: child, onLink: onLink) }
        }
        .overlay(alignment: .top) { Palette.hairline.frame(height: 1).padding(.leading, 60 + CGFloat(row.depth) * 24) }
    }

    private var symbol: String {
        switch row.icon {
        case .mission: "hammer.fill"
        case .action: "hand.raised.fill"
        case .task: row.struck ? "checkmark" : "circle.dashed"
        case .pullRequest: "arrow.triangle.pull"
        case .branch: "link"
        case .check: "checkmark"
        case .warning: "xmark"
        case .request: "paperplane"
        default:
            switch row.badges.first?.tone {
            case .success: "checkmark"
            case .warning, .critical, .yourTurn: "hand.raised.fill"
            default: "circle.dashed"
            }
        }
    }
}

private struct BottomBarModifier<Bar: View>: ViewModifier {
    let enabled: Bool
    @ViewBuilder let bar: () -> Bar

    @ViewBuilder func body(content: Content) -> some View {
        if enabled {
            content.safeAreaBar(edge: .bottom) { bar() }
        } else {
            content
        }
    }
}
