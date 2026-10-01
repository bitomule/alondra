import AlondraShared
import AlondraViewsWorkspaceFeature
import ComposableArchitecture
import SwiftUI
import AlondraViews
import AlondraViewsUI
import AppKit

struct MacServiceWorkspace<AccountContent: View>: View {
    @Bindable var store: StoreOf<ViewsServiceRootFeature>
    let accountContent: AccountContent
    var isActive = true
    private var presentation: StoreOf<MacPresentationFeature> { store.scope(state: \.macPresentation, action: \.macPresentation) }
    @FocusState private var searchFocused: Bool

    init(store: StoreOf<ViewsServiceRootFeature>, accountContent: AccountContent, isActive: Bool = true) {
        self.store = store
        self.accountContent = accountContent
        self.isActive = isActive
    }

    var body: some View {
        NavigationSplitView {
            MacServiceSidebar(presentation: presentation)
                .navigationSplitViewColumnWidth(min: 260, ideal: 320, max: 440)
                .searchable(text: Binding(get: { presentation.query }, set: {
                    presentation.send(.queryChanged($0))
                    presentation.send(.searchRequested)
                }), placement: .sidebar, prompt: "Search")
                .searchFocused($searchFocused)
        } detail: {
            ZStack {
                MacServiceDetail(presentation: presentation)
                    .id(presentation.selection)
                    .transition(.asymmetric(insertion: .opacity.combined(with: .offset(y: 6)), removal: .opacity))
            }
            .animation(.smooth(duration: 0.22), value: presentation.selection)
            .overlay(alignment: .top) { ServiceDeliveryBanner(store: store) }
        }
        .environment(\.searchActive, searchFocused)
        .navigationTitle("Alondra")
        .navigationSubtitle(presentation.sidebarDocument != nil && presentation.viewError != nil
            ? String(localized: "saved data · connection lost")
            : presentation.sidebarDocument?.status.summary ?? String(localized: "Connecting to the fleet…"))
        .toolbar {
            if let status = presentation.sidebarDocument?.status {
                ToolbarItem(placement: .status) { MacViewStatusGauges(gauges: status.gauges) }
                    .sharedBackgroundVisibility(.hidden)
            }
            ToolbarItemGroup(placement: .primaryAction) {
                Button { presentation.send(.select(.viewKey("new_request"))) } label: { Label("New", systemImage: "square.and.pencil") }
                    .keyboardShortcut("n").help("Ask for new work (⌘N)")
            }
        }
        .focusedSceneValue(\.workspaceSettingsStore, isActive ? store : nil)
        .sheet(isPresented: Binding(get: { store.settingsPresented }, set: { store.send(.settingsPresented($0)) })) {
            accountContent.frame(minWidth: 600, minHeight: 520)
        }
        .task { guard isActive else { return }; await store.send(.appeared).finish(); presentation.send(.appeared) }
        .onDisappear {
            presentation.send(.disappeared)
            guard isActive else { return }
            store.send(.disappeared)
        }
        .frame(minWidth: 860, minHeight: 540)
    }
}

private struct MacServiceSidebar: View {
    let presentation: StoreOf<MacPresentationFeature>
    var body: some View {
        List(selection: Binding(get: { presentation.selection }, set: { presentation.send(.select($0)) })) {
            if let document = presentation.sidebarDocument {
                if !presentation.smartStatus.isEmpty {
                    Label(presentation.smartStatus, systemImage: "text.magnifyingglass").font(.callout).foregroundStyle(.secondary)
                }
                ForEach(document.sections, id: \.id) { section in
                    Section(isExpanded: Binding(
                        get: { sidebarSectionExpanded(section) },
                        set: { presentation.send(.sidebarSectionExpanded(section.id, $0)) }
                    )) {
                        ForEach(visibleRows(section), id: \.id) { row in
                            MacViewSidebarRow(row: row, presentation: presentation, filterText: presentation.query)
                        }
                    } header: {
                        sectionHeader(section)
                    }
                }
            } else if let error = presentation.viewError {
                VStack(spacing: 10) {
                    ContentUnavailableView("Couldn’t load the sidebar", systemImage: "exclamationmark.triangle", description: Text(error))
                    Button("Retry") { presentation.send(.appeared) }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ProgressView("Connecting to the fleet…").frame(maxWidth: .infinity)
            }
        }
        .animation(.snappy, value: presentation.sidebarDocument?.version)
    }

    private func sidebarSectionExpanded(_ section: ViewSection) -> Bool {
        if section.id == "tasks" { return presentation.tasksExpanded }
        if section.id.hasPrefix("project:") {
            return presentation.withState { $0.projectExpanded(String(section.id.dropFirst("project:".count))) }
        }
        return !section.collapsed
    }

    private func visibleRows(_ section: ViewSection) -> [Row] {
        let rows = section.items.compactMap { item -> Row? in
            guard case let .row(row) = item else { return nil }
            return row
        }
        return presentation.withState { state in
            if section.id.hasPrefix("project:"), let matches = state.smartMatches {
                return rows.filter { matches.contains($0.id) }
            }
            guard !state.query.isEmpty else { return rows }
            return rows.filter { row in
                let childText = row.children.map { [$0.title, $0.subtitle ?? ""].joined(separator: " ") }.joined(separator: " ")
                return state.matches(([row.title, row.subtitle ?? ""] + row.badges.map(\.text) + [childText]).joined(separator: " "))
            }
        }
    }

    @ViewBuilder private func sectionHeader(_ section: ViewSection) -> some View {
        if section.id.hasPrefix("project:") {
            let rows = visibleRows(section)
            let urgent = rows.filter { row in
                if case let .count(count)? = row.trailing { return count > 0 }
                return false
            }.count
            HStack(spacing: 6) {
                Text(section.title ?? String(section.id.dropFirst("project:".count)))
                if urgent > 0 {
                    Circle().fill(.orange).frame(width: 6, height: 6)
                        .help("\(urgent) nodes waiting")
                }
            }
        } else {
            SidebarCountHeader(title: section.title ?? section.id.capitalized,
                               count: Int(section.count ?? 0), tint: sectionTint(section.id))
        }
    }
}

private func sectionTint(_ id: String) -> Color {
    switch id {
    case "waiting": .orange
    case "pull_requests": .purple
    case "tasks": .gray
    case "orchestrator": .orange
    default: .secondary
    }
}

private struct MacServiceDetail: View {
    let presentation: StoreOf<MacPresentationFeature>
    @Environment(\.openURL) private var openURL
    var body: some View {
        Group {
            if let document = presentation.detailDocument {
                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                        if let title = document.title { Text(title).font(.title2.weight(.semibold)) }
                        ForEach(document.sections, id: \.id) { section in
                            SectionView(section: section,
                                        onAction: perform,
                                        onLink: openLink,
                                        onMore: { presentation.send(.select(.viewKey($0))) },
                                        onForm: { presentation.send(.beginViewForm($0)) },
                                        onContent: { presentation.send(.contentRequested($0)) },
                                        onAttachment: { presentation.send(.attachmentRequested($0)) })
                        }
                    }.frame(maxWidth: 900, alignment: .leading).padding(24)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                .overlay(alignment: .top) {
                    if let error = presentation.viewError {
                        Text(error).font(.callout).foregroundStyle(.red).padding(10)
                            .background(.regularMaterial, in: .capsule).padding(.top, 10)
                    }
                }
                .overlay {
                    VStack(spacing: 0) {
                        ForEach(Array(keyedActions(document).enumerated()), id: \.offset) { _, action in
                            if let key = action.key, key.count == 1, let character = key.first {
                                Button(action.label) { perform(action) }
                                    .keyboardShortcut(KeyEquivalent(character), modifiers: [])
                                    .hidden().frame(width: 0, height: 0)
                            }
                        }
                    }.accessibilityHidden(true)
                }
                .sheet(isPresented: Binding(get: { presentation.actionPresented }, set: { presentation.send(.actionPresented($0)) })) {
                    MacViewActionSheet(presentation: presentation, form: presentation.activeForm,
                                       onDismiss: { presentation.send(.actionPresented(false)) })
                        .frame(minWidth: 420, minHeight: 240)
                }
                .sheet(isPresented: Binding(get: { presentation.contentPresented }, set: { presentation.send(.contentPresented($0)) })) {
                    MacContentViewer(title: presentation.contentTitle, contentText: presentation.contentBody,
                                     isDiff: presentation.contentIsDiff, loading: presentation.contentLoading,
                                     error: presentation.contentError, imageData: presentation.contentImageData,
                                     comments: Binding(get: { presentation.diffComments },
                                                       set: { presentation.send(.diffCommentsChanged($0)) }))
                        .id(presentation.contentDigest)
                        .frame(minWidth: 620, minHeight: 420)
                }
            } else if let error = presentation.viewError {
                ContentUnavailableView("Couldn’t load this view", systemImage: "exclamationmark.triangle", description: Text(error))
            } else { ProgressView("Loading…") }
        }
    }

    private func perform(_ action: AlondraViews.Action) {
        if let link = action.link { openLink(link) }
        else { presentation.send(.beginViewAction(action)) }
    }

    private func openLink(_ link: AlondraViews.Link) {
        switch link {
        case let .view(key): presentation.send(.select(.viewKey(key)))
        case let .url(raw):
            guard let url = URL(string: raw), url.scheme?.lowercased() == "https",
                  let host = url.host?.lowercased(), host == "github.com" || host.hasSuffix(".github.com") else { return }
            openURL(url)
        case let .local(local):
            if case let .attach(session) = local {
                presentation.send(.select(.viewKey("card:\(session)")))
            }
        }
    }

    private func keyedActions(_ document: ViewDocument) -> [AlondraViews.Action] {
        document.sections.flatMap(\.items).flatMap(keyedActions)
    }
    private func keyedActions(_ item: ViewItem) -> [AlondraViews.Action] {
        switch item {
        case let .card(card): return card.actions + card.children.flatMap { $0.actions }
        case let .row(row): return row.actions + row.children.flatMap { $0.actions }
        case let .actions(bar): return bar.actions
        case let .form(form): return [form.submit]
        case let .notice(notice): return [notice.action].compactMap { $0 }
        default: return []
        }
    }
}

private struct MacContentViewer: View {
    let title: String
    let contentText: String
    let isDiff: Bool
    let loading: Bool
    let error: String?
    let imageData: Data?
    @Binding var comments: [String: ActionComment]

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(title).font(.title3.weight(.semibold))
            if loading { ProgressView("Loading…") }
            else if let error { ContentUnavailableView("Couldn’t load content", systemImage: "exclamationmark.triangle", description: Text(error)) }
            else if let imageData, let image = NSImage(data: imageData) {
                Image(nsImage: image).resizable().scaledToFit().frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            else if isDiff {
                ScrollView([.vertical, .horizontal]) {
                    MacViewsPRDiff(text: contentText, comments: $comments)
                        .padding(12)
                }.background(.quaternary.opacity(0.25), in: .rect(cornerRadius: 8))
            } else {
                ScrollView([.vertical, .horizontal]) {
                    Text(contentText).font(.body)
                        .frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled).padding(12)
                }.background(.quaternary.opacity(0.25), in: .rect(cornerRadius: 8))
            }
        }.padding(20).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }
}

private struct MacViewActionSheet: View {
    let presentation: StoreOf<MacPresentationFeature>
    let form: AlondraViews.Form?
    let onDismiss: () -> Void
    private var store: StoreOf<ActionFeature> {
        presentation.scope(state: \.actionFeature, action: \.action)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            if let action = store.action {
                Text(action.label).font(.title3.weight(.semibold))
                ActionInputView(store: store, form: form,
                                showCommentEditor: store.action?.comments?.kind != .diffLines,
                                onReload: { field, value in presentation.send(.reloadForm(field.id, value)) },
                                onPicker: { field, source in presentation.send(.pickerOpened(field.id, source)) })
                HStack {
                    Spacer()
                    Button(store.stamp == nil ? "Cancel" : "Done") { store.send(.dismiss); onDismiss() }.keyboardShortcut(.cancelAction)
                    Button(action.label) { store.send(.submit) }
                        .buttonStyle(.borderedProminent)
                        .disabled(!action.enabled || store.inFlight)
                }
            }
        }
        .padding(24)
        .sheet(isPresented: Binding(get: { presentation.pickerFieldID != nil },
                                    set: { if !$0 { presentation.send(.pickerClosed) } })) {
            MacViewIssuePicker(presentation: presentation)
                .frame(minWidth: 440, minHeight: 360)
        }
        .confirmationDialog(store.confirmation?.title ?? "", isPresented: Binding(get: { store.confirmation != nil }, set: { if !$0 { store.send(.cancelConfirmation) } })) {
            if let confirmation = store.confirmation {
                Button(confirmation.button, role: .destructive) { store.send(.confirm) }
                Button("Cancel", role: .cancel) { store.send(.cancelConfirmation) }
            }
        } message: {
            if let confirmation = store.confirmation { Text(confirmation.message) }
        }
        .alert("The item changed", isPresented: Binding(get: { store.changed != nil }, set: { if !$0 { store.send(.dismiss) } })) {
            Button("Continue", role: .destructive) { store.send(.sendChangedAction) }
            Button("Cancel", role: .cancel) { store.send(.dismiss); onDismiss() }
        } message: {
            if let changed = store.changed { Text(changed.button) }
        }
    }
}

private struct MacViewIssuePicker: View {
    let presentation: StoreOf<MacPresentationFeature>
    private var rows: [Row] {
        presentation.pickerDocument?.sections.flatMap(\.items).compactMap { item in
            if case let .row(row) = item { row } else { nil }
        } ?? []
    }
    var body: some View {
        NavigationStack {
            List {
                if let error = presentation.pickerError {
                    Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.secondary)
                }
                ForEach(rows, id: \.id) { row in
                    Button { presentation.send(.pickerSelected(row.id)) } label: {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(row.title)
                            if let subtitle = row.subtitle { Text(subtitle).font(.caption).foregroundStyle(.secondary) }
                        }.frame(maxWidth: .infinity, alignment: .leading)
                    }.buttonStyle(.plain)
                }
                if rows.isEmpty, let empty = presentation.pickerDocument?.empty {
                    Text(empty.text).foregroundStyle(.secondary)
                }
            }
            .overlay { if presentation.pickerLoading && rows.isEmpty { ProgressView() } }
            .searchable(text: Binding(get: { presentation.pickerQuery },
                                      set: { presentation.send(.pickerQueryChanged($0)) }),
                        prompt: Text("Filter issues"))
            .navigationTitle(presentation.pickerDocument?.title ?? String(localized: "Issues"))
            .toolbar { ToolbarItem(placement: .cancellationAction) {
                Button("Cancel") { presentation.send(.pickerClosed) }
            } }
        }
    }
}

private struct MacViewSidebarRow: View {
    let row: Row
    let presentation: StoreOf<MacPresentationFeature>
    let filterText: String
    @ViewBuilder var body: some View {
        if let selectionTag { rowContent.tag(selectionTag) }
        else { rowContent }
    }
    @ViewBuilder private var rowContent: some View {
        Group {
            if row.icon == .node && !row.children.isEmpty {
                VStack(alignment: .leading, spacing: 3) {
                    rowLabel
                    ForEach(visibleChildren, id: \.id) { child in
                        if let selection = selectionTag(for: child) {
                            Button { presentation.send(.select(selection)) } label: {
                                MacNodeTaskRow(row: child)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .contentShape(.rect)
                            }
                            .buttonStyle(.plain)
                        } else {
                            MacNodeTaskRow(row: child)
                        }
                    }
                }
            } else if row.children.isEmpty {
                rowLabel
            } else {
                DisclosureGroup(isExpanded: Binding(
                    get: { presentation.withState { $0.rowExpanded(row.id, defaultExpanded: true) } },
                    set: { presentation.send(.rowExpanded(row.id, defaultExpanded: true, expanded: $0)) }
                )) {
                    ForEach(visibleChildren, id: \.id) { child in
                        if let selectionTag = selectionTag(for: child) {
                            MacTaskSidebarRow(row: child).tag(selectionTag)
                        } else {
                            MacTaskSidebarRow(row: child)
                        }
                    }
                } label: { rowLabel }
            }
        }
        .contextMenu {
            ForEach(Array(row.actions.enumerated()), id: \.offset) { _, action in
                Button(role: action.style == .destructive ? .destructive : nil) {
                    presentation.send(.beginViewAction(action))
                } label: { Text(action.label) }
                    .disabled(!action.enabled)
            }
        }
    }
    @ViewBuilder private var rowLabel: some View {
        if row.icon == .task && !row.children.isEmpty {
            HStack(spacing: 7) {
                StatusDot(color: taskTint(row))
                Text(row.title).fontWeight(.semibold)
                Spacer(minLength: 4)
                trailingLabel
            }.font(.callout)
        } else if row.icon == .node || row.icon == .orchestrator {
            HStack(spacing: 8) {
                StatusDot(color: nodeTint).frame(width: 16)
                Text(shortName(row.title)).lineLimit(1)
                if row.icon == .orchestrator { Image(systemName: "sparkle").font(.caption2).foregroundStyle(.secondary) }
                if let subtitle = row.subtitle {
                    Spacer(minLength: 4)
                    Text(verbatim: subtitle).font(.caption).foregroundStyle(nodeTint == .yellow ? .orange : .secondary).lineLimit(1)
                }
                if let badge = row.badges.first {
                    Text(badge.text).font(.caption)
                        .foregroundStyle(badge.tone == .warning || badge.tone == .critical ? .orange : .secondary)
                        .lineLimit(1)
                }
                trailingLabel
            }
            .help(([row.title, row.subtitle].compactMap { $0 } + row.badges.map(\.text)).filter { !$0.isEmpty }.joined(separator: " · "))
            .accessibilityElement(children: .combine)
        } else {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 8) {
                Image(systemName: symbol).foregroundStyle(tint).frame(width: 16)
                Text(row.title).fontWeight(.medium).strikethrough(row.struck).lineLimit(2)
                Spacer(minLength: 4)
                trailingLabel
            }
            if let subtitle = row.subtitle { Text(subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1).padding(.leading, 24) }
            if !row.badges.isEmpty { Text(row.badges.map(\.text).joined(separator: " · ")).font(.caption).foregroundStyle(.secondary).padding(.leading, 24).lineLimit(1) }
        }
        .padding(.vertical, 3)
        .accessibilityElement(children: .combine)
        }
    }
    private var selectionTag: MacPresentationFeature.Selection? {
        guard case let .view(key)? = row.link else { return nil }
        return .viewKey(key)
    }
    @ViewBuilder private var trailingLabel: some View {
        if let trailing = row.trailing {
            Group {
                switch trailing {
                case let .count(value): Text(value, format: .number)
                case let .time(at, format): Text(ViewsDate.display(at, format: format))
                case let .text(value): Text(value)
                }
            }.font(.caption.monospacedDigit()).foregroundStyle(.secondary)
        }
    }
    private var visibleChildren: [Row] {
        guard !filterText.isEmpty else { return row.children }
        return row.children.filter { child in
            let text = ([child.title, child.subtitle ?? ""] + child.badges.map(\.text)).joined(separator: " ")
            return text.localizedCaseInsensitiveContains(filterText)
        }
    }
    private var symbol: String {
        switch row.icon {
        case .question: "questionmark.bubble.fill"
        case .action: "hand.point.right.fill"
        case .document: "doc.text.fill"
        case .pullRequest: "arrow.triangle.pull"
        case .node: "cpu"
        case .orchestrator: "point.3.connected.trianglepath.dotted"
        case .mission: "scope"
        case .request: "paperplane"
        case .schedule: "calendar.badge.clock"
        case .task: "checklist"
        default: "bell.fill"
        }
    }
    private var tint: Color {
        switch row.icon {
        case .question: .orange
        case .action: .pink
        case .document: .blue
        case .pullRequest: .green
        case .notice: .purple
        case .node: nodeTint
        case .task: .secondary
        default: .secondary
        }
    }
    private var nodeTint: Color {
        guard let tone = row.badges.first?.tone else { return .gray }
        return switch tone {
        case .neutral: .gray
        case .info: .indigo
        case .accent: .orange
        case .success: .green
        case .warning: .yellow
        case .critical: .red
        }
    }
    private func taskTint(_ row: Row) -> Color {
        color(forTaskRow: row)
    }
    private func selectionTag(for row: Row) -> MacPresentationFeature.Selection? {
        guard case let .view(key)? = row.link else { return nil }
        return .viewKey(key)
    }
}

private struct MacTaskSidebarRow: View {
    let row: Row
    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 7) {
                StatusDot(color: taskColor).frame(width: 6)
                Text(row.title).fontWeight(.medium).strikethrough(row.struck).lineLimit(2)
            }
            if let subtitle = row.subtitle { Text(verbatim: subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1) }
        }.padding(.vertical, 2).accessibilityElement(children: .combine)
    }
    private var taskColor: Color {
        color(forTaskRow: row)
    }
}

private struct MacNodeTaskRow: View {
    let row: Row
    var body: some View {
        HStack(spacing: 7) {
            StatusDot(color: row.struck ? .gray : taskTint).frame(width: 6)
            Text(row.title).strikethrough(row.struck).lineLimit(1)
        }
        .font(.caption).foregroundStyle(.secondary).padding(.leading, 19)
        .accessibilityElement(children: .combine)
    }
    private var taskTint: Color {
        color(forTaskRow: row)
    }
}

private func color(forTaskRow row: Row) -> Color {
        if row.struck { return .gray }
        switch row.icon {
        case .mission: return .green
        case .action: return .pink
        case .pullRequest: return .mint
        case .branch: return .indigo
        case .check: return .gray
        case .warning: return .gray
        default: break
        }
        guard let tone = row.badges.first?.tone else { return .gray }
        switch tone {
        case .neutral: return .gray
        case .info: return .indigo
        case .accent: return .orange
        case .success: return .green
        case .warning: return .orange
        case .critical: return .red
        }
}

private struct MacViewStatusGauges: View {
    let gauges: [WireGauge]
    var body: some View {
        HStack(spacing: 12) {
            ForEach(Array(gauges.enumerated()), id: \.offset) { _, gauge in
                HStack(spacing: 5) {
                    Capsule().fill(.quaternary).frame(width: 28, height: 4).overlay(alignment: .leading) {
                        Capsule().fill(color(gauge.level)).frame(width: 28 * max(0.06, min(gauge.value, 1)))
                    }
                    Text(gauge.text).font(.caption).monospacedDigit().foregroundStyle(gauge.level == .hot ? .red : .secondary)
                }
                .help(gauge.detail ?? "\(gauge.label): \(gauge.text)")
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("\(gauge.label): \(gauge.text)")
            }
        }.padding(.horizontal, 6)
    }
    private func color(_ level: Level) -> Color {
        switch level { case .calm: .accentColor; case .warm: .orange; case .hot: .red }
    }
}
