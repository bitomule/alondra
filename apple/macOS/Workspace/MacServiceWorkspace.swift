import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI
import AlondraContract
import AppKit

public struct MacServiceWorkspace<AccountContent: View>: View {
    @Bindable var store: StoreOf<MacAppFeature>
    let accountContent: AccountContent
    var isActive = true
    private var presentation: StoreOf<MacPresentationFeature> { store.scope(state: \.macPresentation, action: \.macPresentation) }
    @FocusState private var searchFocused: Bool

    public init(store: StoreOf<MacAppFeature>, accountContent: AccountContent, isActive: Bool = true) {
        self.store = store
        self.accountContent = accountContent
        self.isActive = isActive
    }

    public var body: some View {
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
            .safeAreaInset(edge: .top, spacing: 0) {
                UsageExhaustedWindowBanner(usage: presentation.sidebarDocument?.status.usageExhausted)
            }
            .overlay(alignment: .top) { DeliveryBanner(store: store) }
            .overlay(alignment: .bottom) { MacNoticeToast(store: store) }
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

public struct MacServiceSidebar: View {
    let presentation: StoreOf<MacPresentationFeature>
    public init(presentation: StoreOf<MacPresentationFeature>) { self.presentation = presentation }
    public var body: some View {
        List(selection: Binding(get: { presentation.selection }, set: { presentation.send(.select($0)) })) {
            Section {
                Label("Fleet", systemImage: "square.grid.2x2").tag(MacPresentationFeature.Selection.viewKey(ViewKey.fleet.rawValue))
                Label("Tasks", systemImage: "checklist").tag(MacPresentationFeature.Selection.viewKey(ViewKey.tasks.rawValue))
            }
            if let document = presentation.sidebarDocument {
                if !presentation.smartStatus.isEmpty {
                    Label(presentation.smartStatus, systemImage: "text.magnifyingglass").font(.callout).foregroundStyle(.secondary)
                }
                ForEach(document.sections.filter(hasRows), id: \.id) { section in
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
                if let summary = waitingSummary(document) {
                    MacSidebarSummary(summary: summary) { presentation.send(.select(.viewKey($0))) }
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

    private func hasRows(_ section: ViewSection) -> Bool {
        section.items.contains { if case .row = $0 { true } else { false } }
    }

    /// What waits for the person once the answers already sent are left out.
    private func waitingSummary(_ document: ViewDocument) -> WaitingSummary? {
        guard let waiting = document.sections.first(where: { $0.id == "waiting" }) else {
            let notice = document.sections.flatMap(\.items).compactMap { item -> Notice? in
                if case let .notice(notice) = item { notice } else { nil }
            }.first
            guard let notice, case let .view(key)? = notice.action?.link else { return nil }
            return WaitingSummary(text: notice.text, actionLabel: notice.action?.label ?? "", key: key)
        }
        let open = visibleRows(waiting)
        if let first = open.first, case let .view(key)? = first.link {
            return WaitingSummary(text: String(localized: "\(open.count) things waiting for you"), actionLabel: String(localized: "Start"), key: key)
        }
        return WaitingSummary(text: String(localized: "All caught up"), actionLabel: String(localized: "Ask for Something New…"), key: "new_request")
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
            guard case let .row(row) = item, !presentation.optimistic.hides(row.id) else { return nil }
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

    private func waitingCount(_ section: ViewSection) -> Int {
        let count = Int(section.count ?? 0)
        guard section.id == "waiting" else { return count }
        let hidden = section.items.filter { item in
            if case let .row(row) = item { presentation.optimistic.hides(row.id) } else { false }
        }.count
        return max(count - hidden, 0)
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
        } else if let title = section.title {
            SidebarCountHeader(title: title, count: waitingCount(section), tint: sectionTint(section.id))
        }
    }
}

private struct WaitingSummary {
    let text: String
    let actionLabel: String
    let key: String
}

private struct MacSidebarSummary: View {
    let summary: WaitingSummary
    let open: (String) -> Void
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(summary.text).font(.callout).foregroundStyle(.secondary)
            if !summary.actionLabel.isEmpty {
                Button(summary.actionLabel) { open(summary.key) }.buttonStyle(.link)
            }
        }
        .padding(.vertical, 4)
        .selectionDisabled()
    }
}

private func sectionTint(_ id: String) -> Color {
    switch id {
    case "waiting": .orange
    case "tasks": .gray
    case "orchestrator": .orange
    default: .secondary
    }
}

public struct MacServiceDetail: View {
    let presentation: StoreOf<MacPresentationFeature>
    @Environment(\.openURL) private var openURL
    public init(presentation: StoreOf<MacPresentationFeature>) { self.presentation = presentation }
    public var body: some View {
        Group {
            if let document = presentation.detailDocument {
                ScrollViewReader { proxy in
                    ScrollView {
                        VStack(alignment: .leading, spacing: 18) {
                            if let title = document.title, !showsCard(document) { Text(title).font(.title2.weight(.semibold)) }
                            ForEach(document.sections, id: \.id) { section in
                                MacSectionView(section: section,
                                               returnReasons: presentation.returnReasons,
                                               onAction: perform,
                                               onReply: { presentation.send(.sendReply($0, text: $1)) },
                                               onLink: openLink,
                                               onMore: { key in
                                                   if section.collapsed && section.items.isEmpty {
                                                       presentation.send(.sectionMore(section.id, key: key))
                                                   } else {
                                                       presentation.send(.select(.viewKey(key)))
                                                   }
                                               },
                                               onForm: { presentation.send(.beginViewForm($0)) },
                                               onContent: { presentation.send(.contentRequested($0)) },
                                               onAttachment: { presentation.send(.attachmentRequested($0)) },
                                               onContentRow: { showContent(of: document, scrollTo: proxy) },
                                               opened: presentation.openSections.contains(section.id),
                                               page: presentation.sectionPages[section.id],
                                               onToggle: { presentation.send(.sectionToggled(section)) })
                                    .id(section.id)
                            }
                        }.frame(maxWidth: 900, alignment: .leading).padding(24)
                    }
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
            } else if presentation.selectedViewKey == nil {
                ContentUnavailableView("Nothing selected", systemImage: "sidebar.left",
                                       description: Text("Choose something in the sidebar."))
            } else { ProgressView("Loading…") }
        }
    }

    private func showsCard(_ document: ViewDocument) -> Bool {
        document.sections.contains { $0.items.contains { if case let .card(card) = $0 { card.need != nil } else { false } } }
    }

    private func showContent(of document: ViewDocument, scrollTo proxy: ScrollViewProxy) {
        let diff = document.sections.lazy.flatMap(\.items).compactMap { item -> ContentRef? in
            if case let .contentRef(reference) = item, reference.kind == .diff { reference } else { nil }
        }.first
        if let diff {
            presentation.send(.contentRequested(diff))
        } else if document.sections.contains(where: { $0.id == "content" }) {
            withAnimation(.smooth) { proxy.scrollTo("content", anchor: .top) }
        }
    }

    private func perform(_ action: AlondraContract.Action) {
        if let link = action.link { openLink(link) }
        else { presentation.send(.beginViewAction(action)) }
    }

    private func openLink(_ link: AlondraContract.Link) {
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

    private func keyedActions(_ document: ViewDocument) -> [AlondraContract.Action] {
        document.sections.flatMap(\.items).flatMap(keyedActions)
    }
    private func keyedActions(_ item: ViewItem) -> [AlondraContract.Action] {
        switch item {
        case .card: return []
        case let .row(row): return row.actions + row.children.flatMap { $0.actions }
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

public struct MacViewActionSheet: View {
    let presentation: StoreOf<MacPresentationFeature>
    let form: AlondraContract.Form?
    let onDismiss: () -> Void
    public init(presentation: StoreOf<MacPresentationFeature>, form: AlondraContract.Form? = nil, onDismiss: @escaping () -> Void = {}) {
        self.presentation = presentation; self.form = form; self.onDismiss = onDismiss
    }
    private var store: StoreOf<ActionFeature> {
        presentation.scope(state: \.actionFeature, action: \.action)
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            if let action = store.action, action.asksForClarification {
                MacClarifyForm(store: store, action: action, onDismiss: onDismiss)
            } else if let action = store.action {
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
        .actionDialogs(store)
    }
}

extension AlondraContract.Action {
    public var asksForClarification: Bool { id?.hasPrefix("clarify:") == true && presentation == .options }
}

/// The reason for asking the node to clarify, and what it should explain, if anything.
private struct MacClarifyForm: View {
    @Bindable var store: StoreOf<ActionFeature>
    let action: AlondraContract.Action
    let onDismiss: () -> Void
    @FocusState private var noteFocused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text(action.label).font(.title3.weight(.semibold))
                Text("The node replaces this card with a clearer one.").font(.callout).foregroundStyle(.secondary)
            }
            VStack(alignment: .leading, spacing: 6) {
                ForEach(action.options, id: \.id) { option in reason(option) }
            }
            VStack(alignment: .leading, spacing: 6) {
                Text("What should it clarify? (optional)").font(.subheadline.weight(.semibold))
                TextField(placeholder, text: Binding(get: { store.text }, set: { store.send(.textChanged($0)) }), axis: .vertical)
                    .textFieldStyle(.plain).lineLimit(3...6).focused($noteFocused)
                    .padding(10)
                    .background(.quaternary.opacity(0.4), in: .rect(cornerRadius: 8))
            }
            if let error = store.error { Text(error).font(.callout).foregroundStyle(.red) }
            HStack {
                Spacer()
                Button("Cancel") { store.send(.dismiss); onDismiss() }.keyboardShortcut(.cancelAction)
                Button("Request clarification") { store.send(.submit) }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(store.selectedOptionID == nil || !action.enabled || store.inFlight)
            }
        }
    }

    private var placeholder: String {
        if case let .text(placeholder, _, _) = action.input, let placeholder { return placeholder }
        return String(localized: "What should it clarify?")
    }

    private func reason(_ option: ActionOption) -> some View {
        let chosen = store.selectedOptionID == option.id
        return Button { store.send(.chooseOption(option.id)) } label: {
            HStack(spacing: 10) {
                Image(systemName: chosen ? "largecircle.fill.circle" : "circle")
                    .foregroundStyle(chosen ? Color.accentColor : .secondary)
                Text(option.label)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 10).padding(.vertical, 7)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(chosen ? Color.accentColor.opacity(0.12) : .clear, in: .rect(cornerRadius: 8))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(chosen ? .isSelected : [])
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
                    presentation.send(.beginViewAction(action, target: cardTarget))
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
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 8) {
                    StatusDot(color: nodeTint).frame(width: 16)
                    Text(shortName(row.title)).fontWeight(.medium).lineLimit(1)
                    if row.icon == .orchestrator { Image(systemName: "sparkle").font(.caption2).foregroundStyle(.secondary) }
                    Spacer(minLength: 4)
                    trailingLabel
                }
                if let subtitle = row.subtitle {
                    Text(verbatim: subtitle).font(.caption).foregroundStyle(.secondary)
                        .lineLimit(2).fixedSize(horizontal: false, vertical: true).padding(.leading, 24)
                }
                if !row.badges.isEmpty {
                    Text(row.badges.map(\.text).joined(separator: " · ")).font(.caption)
                        .foregroundStyle(nodeTint == .yellow ? .orange : .secondary)
                        .lineLimit(2).fixedSize(horizontal: false, vertical: true).padding(.leading, 24)
                }
            }
            .padding(.vertical, 2)
            .help(([row.title, row.subtitle].compactMap { $0 } + row.badges.map(\.text)).filter { !$0.isEmpty }.joined(separator: " · "))
            .accessibilityElement(children: .combine)
        } else if let kind = row.kind {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(row.badges.first?.text ?? "").font(.caption2.weight(.bold)).kerning(0.8).textCase(.uppercase)
                        .foregroundStyle(MacCardPalette.kind(kind)).lineLimit(1)
                    Spacer(minLength: 4)
                    trailingLabel
                }
                Text(row.title).fontWeight(.medium).strikethrough(row.struck).lineLimit(2)
                if let subtitle = row.subtitle { Text(subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1) }
            }
            .padding(.vertical, 3)
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
            if !row.badges.isEmpty && row.children.isEmpty { Text(row.badges.map(\.text).joined(separator: " · ")).font(.caption).foregroundStyle(.secondary).padding(.leading, 24).lineLimit(1) }
        }
        .padding(.vertical, 3)
        .accessibilityElement(children: .combine)
        }
    }
    private var cardTarget: String? {
        guard case let .view(key)? = row.link, MacPresentationFeature.cardID(forViewKey: key) != nil else { return nil }
        return row.id
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
        case .check: "checkmark.circle"
        case .branch: "arrow.branch"
        case .warning: "exclamationmark.triangle"
        default: "bell.fill"
        }
    }
    private var tint: Color {
        if !row.children.isEmpty { return color(forTaskRow: row) }
        switch row.icon {
        case .question: return .orange
        case .action: return .pink
        case .document: return .blue
        case .pullRequest: return .green
        case .notice: return .purple
        case .node: return nodeTint
        default: return .secondary
        }
    }
    private var nodeTint: Color { SidebarTint.node(row.badges.first?.tone) }
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
        return SidebarTint.task(row.badges.first?.tone)
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
