import AlondraViews
import AlondraViewsDependency
import AlondraViewsUI
import AlondraViewsWorkspaceFeature
import AlondraShared
import ComposableArchitecture
import SwiftUI
import UIKit

struct ViewsScreenView: View {
    @Bindable var store: StoreOf<ViewsScreenFeature>
    let openView: (ViewKey) -> Void
    @Environment(\.openURL) private var openURL
    @Namespace private var cardZoom
    @State private var diffComments: [String: ActionComment] = [:]
    @State private var nodeMessageDraft = ""

    private var isTasksScreen: Bool { store.key.rawValue == "tasks" || store.key.rawValue.hasPrefix("tasks?") }
    private var isNewRequestScreen: Bool { store.key.rawValue == "new_request" || store.key.rawValue.hasPrefix("new_request?") }
    private var isInlineForm: Bool { isNewRequestScreen || store.key.rawValue.hasPrefix("schedule:") }
    private var documentCard: AlondraViews.Card? {
        guard store.key.rawValue.hasPrefix("card:") else { return nil }
        return store.document?.sections.lazy.flatMap(\.items).compactMap { item in
            if case let .card(card) = item, card.icon == .document { card } else { nil }
        }.first
    }
    private var routeCard: AlondraViews.Card? {
        guard store.key.rawValue.hasPrefix("card:") else { return nil }
        return store.document?.sections.lazy.flatMap(\.items).compactMap { item in
            if case let .card(card) = item { card } else { nil }
        }.first
    }
    private var readableCard: AlondraViews.Card? {
        guard let card = routeCard,
              card.icon != .question, card.icon != .action, card.icon != .pullRequest else { return nil }
        return card
    }
    private var isDocumentRoute: Bool { store.key.rawValue.hasPrefix("document:") }
    private var documentContentRef: ContentRef? {
        store.document?.sections.lazy.flatMap(\.items).compactMap { item -> ContentRef? in
            if case let .contentRef(reference) = item, reference.kind == .document { reference } else { nil }
        }.first
    }
    private var documentReviewCard: AlondraViews.Card? {
        guard isDocumentRoute || documentCard != nil, let document = store.document, let content = store.content,
              let actionData = try? ViewsCodec.encode(reviewActions(in: document)),
              let actions = try? JSONSerialization.jsonObject(with: actionData),
              let data = try? JSONSerialization.data(withJSONObject: [
                  "id": "document-review", "icon": "document", "title": document.title ?? "Document",
                  "detail": "included", "body": ["text": content, "truncated": false, "commentable": true],
                  "actions": actions,
              ] as [String: Any]) else { return nil }
        return try? ViewsCodec.decode(AlondraViews.Card.self, from: data)
    }
    private func reviewActions(in document: ViewDocument) -> [AlondraViews.Action] {
        let actions = (documentCard?.actions ?? []) + document.sections.flatMap(\.items).flatMap { item -> [AlondraViews.Action] in
            if case let .actions(bar) = item { bar.actions } else { [] }
        }
        var seen = Set<String>()
        return actions.filter { action in
            guard let id = action.id, !id.isEmpty else { return true }
            return seen.insert(id).inserted
        }
    }
    private func presentReview(_ action: AlondraViews.Action, comments: [ActionComment], text: String) {
        store.send(.action(.present(action)))
        store.send(.action(.commentsChanged(comments)))
        store.send(.action(.textChanged(text)))
    }
    private var pickerRows: [AlondraViews.Row] {
        store.pickerDocument?.sections.flatMap(\.items).compactMap { item in
            if case let .row(row) = item { row } else { nil }
        } ?? []
    }
    private var screenGlow: Color {
        if store.key.rawValue == "fleet" || store.key.rawValue.hasPrefix("node:") { Palette.sky }
        else if isNewRequestScreen { Palette.ember }
        else { Palette.dawn }
    }

    var body: some View { alertsContent }

    private var lifecycleContent: some View {
        screenContent
        .task { await store.send(.appeared).finish() }
        .task(id: store.document?.version) {
            guard let document = store.document else { return }
            if store.key.rawValue.hasPrefix("node:"), let message = nodeMessageAction(document),
               store.action.action?.id != message.id {
                store.send(.action(.present(message)))
            }
            if store.key.rawValue.hasPrefix("pull_request:") || store.key.rawValue.hasPrefix("card:"),
               let diff = document.sections.lazy.flatMap(\.items).compactMap({ item -> ContentRef? in
                    if case let .contentRef(reference) = item, reference.kind == .diff { reference } else { nil }
                }).first {
                if store.contentReference?.digest != diff.digest {
                    diffComments = [:]
                    store.send(.fetchContent(diff))
                }
                return
            }
            if let reference = documentContentRef,
                      store.contentReference?.digest != reference.digest {
                store.send(.fetchContent(reference))
            }
        }
        .onChange(of: store.action.stamp) { old, new in
            if old == nil, new != nil { nodeMessageDraft = "" }
        }
        .onDisappear { store.send(.disappeared) }
    }

    private var attachmentContent: some View {
        lifecycleContent
        .sheet(item: Binding(get: { store.attachmentPreview }, set: { if $0 == nil { store.send(.closeAttachment) } })) { preview in
            ViewsAttachmentPreviewView(preview: preview)
        }
        .alert("Could not open attachment", isPresented: Binding(
            get: { documentCard != nil && store.error != nil },
            set: { if !$0 { store.send(.dismissError) } }
        )) {
            Button("OK") { store.send(.dismissError) }
        } message: {
            Text(store.error ?? "")
        }
    }

    private var actionSheetContent: some View {
        attachmentContent
        .sheet(isPresented: Binding(get: { store.action.action != nil && !isInlineNodeMessage && !isInlineForm }, set: { if !$0 && !isInlineNodeMessage && !isInlineForm { store.send(.action(.dismiss)) } })) {
            ActionInputView(store: store.scope(state: \.action, action: \.action), form: store.form,
                            showCommentEditor: documentCard == nil && !store.key.rawValue.hasPrefix("pull_request:"),
                            onReload: { field, value in store.send(.reloadForm(field.id, value)) },
                            onPicker: { field, source in store.send(.pickerOpened(field.id, source)) })
                .padding()
                .presentationDetents([.medium, .large])
                .safeAreaInset(edge: .bottom) {
                    Button(store.form?.id == "schedule" ? (store.form?.submit.label ?? String(localized: "Save Schedule")) : String(localized: "Continue")) { store.send(.action(.submit)) }
                        .buttonStyle(.borderedProminent).tint(Palette.dawn).padding()
                }
        }
    }

    private var pickerContent: some View {
        actionSheetContent
        .sheet(isPresented: Binding(get: { store.pickerFieldID != nil && !isNewRequestScreen },
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
        .confirmationDialog(store.action.confirmation?.title ?? "Confirm", isPresented: Binding(
            get: { store.action.confirmation != nil },
            set: { if !$0 { store.send(.action(.cancelConfirmation)) } }
        )) {
            Button(store.action.confirmation?.button ?? "Confirm", role: .destructive) { store.send(.action(.confirm)) }
            Button("Cancel", role: .cancel) { store.send(.action(.cancelConfirmation)) }
        } message: {
            Text(store.action.confirmation?.message ?? "")
        }
        .alert("Action updated", isPresented: Binding(get: { store.action.changed != nil }, set: { if !$0 { store.send(.action(.dismiss)) } })) {
            Button("Continue") { store.send(.action(.sendChangedAction)) }
            Button("Cancel", role: .cancel) { store.send(.action(.dismiss)) }
        } message: { Text("The item changed. Confirm to send your response to the current version.") }
    }

    private var screenContent: some View {
        Group {
            if let card = documentReviewCard ?? documentCard {
                ServiceWireDocumentView(card: card,
                                        markdownText: documentContentRef == nil ? nil : (store.content ?? ""),
                                        attachments: store.scope(state: \.action.attachmentPicker, action: { .action(.attachmentPicker($0)) }),
                                        onAction: { presentReview($0, comments: $1, text: $2) },
                                        onLink: follow, onAttachment: { store.send(.fetchAttachment($0)) })
                .id(card.id)
            } else if let card = readableCard {
                ServiceWireDocumentView(card: card,
                                        attachments: store.scope(state: \.action.attachmentPicker, action: { .action(.attachmentPicker($0)) }),
                                        onAction: { presentReview($0, comments: $1, text: $2) },
                                        onLink: follow, onAttachment: { store.send(.fetchAttachment($0)) })
                    .id(card.id)
            } else if let card = routeCard {
                VStack(spacing: 16) {
                    ServiceHomeCard(card: card,
                        actions: store.scope(state: \.action, action: \.action),
                        attachments: store.scope(state: \.action.attachmentPicker, action: { .action(.attachmentPicker($0)) }),
                        isFront: true, zoom: cardZoom,
                        submit: { store.send(.action(.submit)) },
                        openReview: { open(ViewKey(rawValue: $0)) })
                    if store.contentReference?.kind == .diff, let content = store.content {
                        ViewsPRDiff(text: content, comments: $diffComments)
                    }
                }
            } else if store.key.rawValue.hasPrefix("schedule:"), let form = store.form {
                ServiceScheduleEditor(store: store, form: form)
            } else if let document = store.document, store.key.rawValue.hasPrefix("pull_request:") {
                ServiceWirePullRequestView(document: document,
                                           diff: store.contentReference?.kind == .diff ? store.content : nil,
                                           comments: $diffComments,
                                           onAction: { action, optionID in
                    present(action)
                    if let optionID {
                        store.send(.action(.chooseOption(optionID)))
                    }
                }, onLink: follow,
                   onContent: { store.send(.fetchContent($0)) },
                   onAttachment: { store.send(.fetchAttachment($0)) })
            } else if let document = store.document {
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
                        if isTasksScreen || isNewRequestScreen {
                            Text(document.title ?? document.status.summary)
                                .font(.display(.largeTitle, weight: .bold))
                                .foregroundStyle(Palette.ink)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.horizontal, 4)
                                .padding(.top, 4)
                        }
                        ForEach(document.notices, id: \.text) { notice in NoticeView(notice: notice) }
                        if store.key.rawValue == "fleet", store.document != nil, store.error != nil {
                            Label(String(localized: "saved data · connection lost"), systemImage: "wifi.slash")
                                .font(.footnote.weight(.medium)).foregroundStyle(.secondary)
                                .padding(.horizontal, 12).padding(.vertical, 8)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .background(.thinMaterial, in: .rect(cornerRadius: 12))
                                .accessibilityIdentifier("fleet.savedDataConnectionLost")
                        }
                        ForEach(displayedSections(in: document), id: \.id) { section in
                            sectionContent(section)
                        }
                        if store.key.rawValue == "fleet", !store.searchQuery.isEmpty {
                            if store.searchLoading { ProgressView().frame(maxWidth: .infinity).padding() }
                            else if let empty = store.searchDocument?.empty {
                                ContentUnavailableView(empty.text, systemImage: "magnifyingglass")
                            }
                        }
                        if let content = store.content {
                            if store.contentReference?.kind == .diff {
                                ViewsPRDiff(text: content, comments: $diffComments)
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
                .navigationTitle(isTasksScreen || isNewRequestScreen ? "" : (document.title ?? document.status.summary))
                .navigationBarTitleDisplayMode(isTasksScreen || isNewRequestScreen ? .inline : .automatic)
                .refreshable { await store.send(.refresh).finish() }
                .modifier(BottomBarModifier(enabled: hasBottomBar(document)) { documentBottomBar(document) })
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
                .modifier(FleetSearchModifier(enabled: store.key.rawValue == "fleet",
                    text: Binding(get: { store.searchQuery }, set: { store.send(.searchQueryChanged($0)) })))
    }

    private var hidesTabBar: Bool {
        store.key.rawValue.hasPrefix("task:") || store.key.rawValue.hasPrefix("node:")
    }

    private func hasBottomBar(_ document: ViewDocument) -> Bool {
        if store.key.rawValue.hasPrefix("node:") { return nodeMessageAction(document) != nil }
        return isInlineForm && store.form != nil
    }

    @ViewBuilder private func documentBottomBar(_ document: ViewDocument) -> some View {
                    if store.key.rawValue.hasPrefix("node:"), let action = nodeMessageAction(document) {
                        HStack(alignment: .bottom, spacing: 10) {
                            HStack(alignment: .bottom, spacing: 4) {
                                ServiceAttachButton(store: store.scope(state: \.action.attachmentPicker,
                                    action: { .action(.attachmentPicker($0)) }))
                                    .padding(.leading, 6).padding(.bottom, 4)
                                TextField(messagePlaceholder(action), text: $nodeMessageDraft, axis: .vertical)
                                    .lineLimit(1...5)
                                    .padding(.trailing, 16).padding(.vertical, 12)
                            }
                            .glassEffect(.regular.interactive(), in: .rect(cornerRadius: 22))
                            Button {
                                store.send(.action(.textChanged(nodeMessageDraft)))
                                store.send(.action(.submit))
                            } label: {
                                Image(systemName: "arrow.up").font(.body.weight(.bold))
                                    .frame(width: 26, height: 26)
                            }
                            .buttonStyle(.glassProminent).buttonBorderShape(.circle).tint(Palette.dawn)
                            .disabled(nodeMessageDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                            .accessibilityLabel(action.label)
                        }
                        .padding(.horizontal, 16).padding(.vertical, 8)
                    } else if isInlineForm, let form = store.form {
                        if isNewRequestScreen {
                            Button { store.send(.action(.submit)) } label: {
                                Label(store.action.inFlight ? String(localized: "Sending…") : String(localized: "Ask the Orchestrator"),
                                      systemImage: "paperplane.fill")
                                    .font(.headline).frame(maxWidth: .infinity)
                            }
                            .buttonStyle(.glassProminent).buttonBorderShape(.capsule)
                            .controlSize(.large).tint(Palette.ember)
                            .disabled(store.action.inFlight || !form.submit.enabled)
                            .padding(.horizontal, 20).padding(.vertical, 8)
                        } else {
                            Button(form.submit.label) { store.send(.action(.submit)) }
                                .buttonStyle(.borderedProminent).tint(Palette.dawn)
                                .disabled(store.action.inFlight || !form.submit.enabled)
                                .frame(maxWidth: .infinity).padding(.horizontal, 20).padding(.vertical, 8)
                        }
                    }
    }

    private func follow(_ link: AlondraViews.Link) {
        switch link {
        case .view(let key): open(ViewKey(rawValue: key))
        case .url(let raw):
            guard let url = URL(string: raw), url.scheme?.lowercased() == "https",
                  let host = url.host?.lowercased(), host == "github.com" || host.hasSuffix(".github.com") else { return }
            openURL(url)
        case .local(let local):
            if case let .attach(session) = local { open(.card(session)) }
        }
    }

    private func open(_ key: ViewKey) {
        openView(key)
    }

    private func present(_ action: AlondraViews.Action) {
        store.send(.action(.present(action)))
        if action.comments?.kind == .diffLines {
            store.send(.action(.commentsChanged(diffComments.values.sorted { $0.anchor < $1.anchor })))
        }
    }

    @ViewBuilder private func sectionContent(_ section: ViewSection) -> some View {
        if isInlineForm, section.id == "form", let form = store.form {
            ActionInputView(store: store.scope(state: \.action, action: \.action), form: form,
                            onReload: { field, value in store.send(.reloadForm(field.id, value)) },
                            onPicker: { field, source in store.send(.pickerOpened(field.id, source)) },
                            pickerPresented: Binding(get: { store.pickerFieldID != nil },
                                                     set: { if !$0 { store.send(.pickerClosed) } }),
                            pickerSheet: { AnyView(issuePickerPanel) })
                .padding(4)
        } else if store.key.rawValue.hasPrefix("task:"), section.id == "header" {
            taskHeader(section)
        } else if store.key.rawValue.hasPrefix("task:"), section.id == "now" {
            taskNow(section)
        } else if store.key.rawValue.hasPrefix("task:"), section.id == "waiting" {
            taskWaiting(section)
        } else if store.key.rawValue.hasPrefix("node:"), section.id == "waiting" {
            if !section.items.isEmpty { taskWaiting(section) }
        } else if store.key.rawValue.hasPrefix("node:"), section.id == "since" {
            EmptyView()
        } else if store.key.rawValue.hasPrefix("node:"), section.id == "header" {
            nodeHeader(section)
        } else if store.key.rawValue.hasPrefix("node:"), section.id == "last_message" {
            nodeLastMessage(section)
        } else if isTasksScreen, ["active", "finished", "archived"].contains(section.id) {
            if !section.collapsed {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(alignment: .firstTextBaseline) {
                        if let title = section.title {
                            Text(title).font(.display(.title3, weight: .bold)).foregroundStyle(Palette.ink)
                        }
                        Spacer()
                        if let count = section.count, count > 0 { Text("\(count)").font(.caption.monospacedDigit()).foregroundStyle(.secondary) }
                    }
                    let rows = section.items.compactMap { item -> AlondraViews.Row? in
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
        } else if store.key.rawValue == "fleet",
                  section.id == "nodes" || section.id == "orchestrator" || section.id == "results" || section.id.hasPrefix("project:") {
            VStack(alignment: .leading, spacing: 8) {
                if let title = section.title {
                    Text(title).font(.display(.title3, weight: .bold)).foregroundStyle(Palette.ink)
                }
                let rows = section.items.compactMap { item -> AlondraViews.Row? in
                    if case let .row(row) = item { row } else { nil }
                }
                ForEach(rows, id: \.id) { row in FleetWireRow(row: row, onLink: follow) }
            }
        } else if isTasksScreen, section.id == "actions", !hasFinishedRows {
            let actions = section.items.flatMap { item -> [AlondraViews.Action] in
                if case let .actions(bar) = item { bar.actions } else { [] }
            }
            if !actions.isEmpty && actions.allSatisfy({ $0.verb == "archive_done" }) {
                EmptyView()
            } else {
                SectionView(section: section, onAction: present, onLink: follow,
                            onMore: { open(ViewKey(rawValue: $0)) }, onForm: presentForm,
                            onContent: { store.send(.fetchContent($0)) }, onAttachment: { store.send(.fetchAttachment($0)) })
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
                        onAttachment: { store.send(.fetchAttachment($0)) },
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
                    if case let .view(key) = fact.link, key.hasPrefix("node:") {
                        Text(fact.value).font(.mono(.caption)).foregroundStyle(.secondary)
                    } else {
                        Text("from “\(fact.value)”").font(.subheadline).foregroundStyle(.secondary)
                    }
                }.buttonStyle(.plain)
            }
        }
    }

    private func taskNow(_ section: ViewSection) -> some View {
        let row = section.items.compactMap { item -> AlondraViews.Row? in
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
        let cards = section.items.compactMap { item -> AlondraViews.Card? in
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

    private func nodeHeader(_ section: ViewSection) -> some View {
        let state = section.items.compactMap { item -> String? in
            guard case let .facts(facts) = item else { return nil }
            return facts.items.first?.value
        }.first ?? ""
        let since = store.document?.sections.first(where: { $0.id == "since" })?.items.compactMap { item -> String? in
            guard case let .row(row) = item, let trailing = row.trailing,
                  case let .time(at, _) = trailing else { return nil }
            return relativeSince(at)
        }.first
        return VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                Glyph(symbol: "desktopcomputer", tint: Palette.sky, size: .small)
                Text(state.prefix(1).uppercased() + state.dropFirst())
                    .font(.subheadline.weight(.semibold)).foregroundStyle(Palette.sky)
                if let since { Text("since \(since)").font(.subheadline).foregroundStyle(.secondary) }
            }
            Text(store.document?.title ?? "")
                .font(.display(.largeTitle, weight: .bold)).foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func nodeLastMessage(_ section: ViewSection) -> some View {
        let message = section.items.compactMap { item -> Markdown? in
            if case let .markdown(markdown) = item { markdown } else { nil }
        }.first
        let time = section.items.compactMap { item -> String? in
            guard case let .row(row) = item, let trailing = row.trailing,
                  case let .time(at, format) = trailing else { return nil }
            return ViewsDate.display(at, format: format)
        }.first
        return VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label(section.title ?? String(localized: "What it said last"), systemImage: "text.quote")
                    .font(.footnote.weight(.bold)).textCase(.uppercase).foregroundStyle(.secondary)
                Spacer()
                if let time { Text(time).font(.caption).foregroundStyle(.tertiary) }
            }
            if let message {
                MarkdownView(markdown: message)
                    .frame(maxWidth: .infinity, alignment: .leading).padding(16)
                    .background(Palette.card.opacity(0.9), in: .rect(cornerRadius: 20))
            }
        }
    }

    private func presentForm(_ form: WireForm) {
        store.send(.presentForm(form))
    }

    private var hasFinishedRows: Bool {
        store.document?.sections.first(where: { $0.id == "finished" })?.items.contains(where: {
            if case .row = $0 { true } else { false }
        }) == true
    }

    private func nodeMessageAction(_ document: ViewDocument) -> AlondraViews.Action? {
        document.sections.flatMap(\.items).flatMap { item -> [AlondraViews.Action] in
            if case let .actions(bar) = item { bar.actions } else { [] }
        }.first { $0.verb == "msg" }
    }

    private func displayedSections(in document: ViewDocument) -> [ViewSection] {
        guard store.key.rawValue == "fleet", !store.searchQuery.isEmpty else { return document.sections }
        return store.searchDocument?.sections ?? []
    }

    private var isInlineNodeMessage: Bool {
        guard store.key.rawValue.hasPrefix("node:"), let action = store.action.action,
              let document = store.document, let message = nodeMessageAction(document) else { return false }
        return action.id == message.id
    }

    private func messagePlaceholder(_ action: AlondraViews.Action) -> String {
        if case let .text(placeholder, _, _) = action.input, let placeholder { return placeholder }
        return String(localized: "Write a message")
    }

    private func excludedActionIDs(for section: ViewSection) -> Set<String> {
        guard let document = store.document else { return [] }
        if store.key.rawValue.hasPrefix("node:"), section.id == "actions",
           let message = nodeMessageAction(document), let id = message.id { return [id] }
        guard store.key.rawValue.hasPrefix("pull_request:"), section.id != "actions" else { return [] }
        return Set(document.sections.flatMap(\.items).flatMap { item -> [AlondraViews.Action] in
            if case let .actions(bar) = item { bar.actions } else { [] }
        }.compactMap(\.id))
    }
}

private struct ViewsAttachmentPreviewView: View {
    let preview: ViewsAttachmentPreview
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView {
                Group {
                    if preview.attachment.kind == .image, let image = UIImage(data: preview.data) {
                        Image(uiImage: image).resizable().scaledToFit()
                    } else if preview.attachment.kind == .text,
                              let text = String(data: preview.data, encoding: .utf8) {
                        Text(text).font(.system(.body, design: .monospaced))
                            .textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
                    } else {
                        ContentUnavailableView("Preview unavailable", systemImage: "doc")
                    }
                }
                .padding(20)
            }
            .background(DawnBackground())
            .navigationTitle(preview.attachment.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
    }
}

private struct FleetWireRow: View {
    let row: AlondraViews.Row
    let onLink: (AlondraViews.Link) -> Void

    var body: some View {
        VStack(spacing: 0) {
            Button { if let link = row.link { onLink(link) } } label: {
                HStack(alignment: .top, spacing: 12) {
                Glyph(symbol: row.icon == .orchestrator ? "point.3.connected.trianglepath.dotted" : "desktopcomputer",
                      tint: fleetTint, size: .small)
                VStack(alignment: .leading, spacing: 4) {
                    Text(row.title).font(.mono(.subheadline, weight: .semibold)).foregroundStyle(Palette.ink)
                        .lineLimit(1).truncationMode(.middle)
                    if let subtitle = row.subtitle {
                        Text(subtitle).font(.caption.weight(.semibold)).foregroundStyle(fleetTint).lineLimit(2)
                    }
                    if !row.badges.isEmpty {
                        Text(row.badges.map(\.text).joined(separator: " · "))
                            .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                }
                Spacer(minLength: 0)
                if case let .count(value) = row.trailing, value > 0 {
                    Text(value, format: .number).font(.caption.weight(.bold).monospacedDigit())
                        .foregroundStyle(Palette.onAccent).frame(minWidth: 22, minHeight: 22)
                        .background(Palette.dawn, in: .capsule)
                }
                }
                .padding(.horizontal, 14).padding(.vertical, 10)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .buttonStyle(.plain)
            ForEach(row.children, id: \.id) { child in
                if child.id == "last_message" {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(child.title).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                        Spacer(minLength: 0)
                        if let trailing = child.trailing, case let .time(at, format) = trailing {
                            Text(ViewsDate.display(at, format: format))
                                .font(.caption2).foregroundStyle(.tertiary)
                        }
                    }
                    .padding(.horizontal, 14).padding(.bottom, 12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                } else {
                    TaskWireRow(row: child, onLink: onLink)
                }
            }
        }
        .background(Palette.card.opacity(0.85), in: .rect(cornerRadius: 16, style: .continuous))
    }

    private var fleetTint: Color {
        if row.icon == .orchestrator { return Palette.ember }
        return switch row.badges.first?.tone {
        case .success: Palette.sky
        case .critical: Palette.rose
        default: Palette.sage
        }
    }
}

private struct TaskRequestRow: View {
    let row: AlondraViews.Row
    let onLink: (AlondraViews.Link) -> Void

    var body: some View {
        VStack(spacing: 0) {
            Button { if let link = row.link { onLink(link) } } label: {
                HStack(alignment: .top, spacing: 10) {
                    VStack(alignment: .leading, spacing: 5) {
                        if !row.badges.isEmpty {
                            Text(row.badges.map(\.text).joined(separator: " · "))
                                .font(.caption.weight(.medium)).foregroundStyle(.secondary).lineLimit(1)
                        }
                        Text(row.title).font(.display(.title3, weight: .bold)).foregroundStyle(Palette.ink)
                            .multilineTextAlignment(.leading).fixedSize(horizontal: false, vertical: true)
                        if let subtitle = row.subtitle { Text(subtitle).font(.caption).foregroundStyle(.secondary) }
                    }
                    Spacer(minLength: 0)
                    if case let .time(at, format) = row.trailing { Text(ViewsDate.display(at, format: format)).font(.caption).foregroundStyle(.tertiary) }
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
    let row: AlondraViews.Row
    let onLink: (AlondraViews.Link) -> Void
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
            case .warning, .critical: "hand.raised.fill"
            default: "circle.dashed"
            }
        }
    }
}

private extension AlondraViews.Action {
    var verb: String? {
        guard let id, let separator = id.firstIndex(of: ":") else { return nil }
        return String(id[..<separator])
    }
}

private struct FleetSearchModifier: ViewModifier {
    let enabled: Bool
    @Binding var text: String

    @ViewBuilder func body(content: Content) -> some View {
        if enabled {
            content.searchable(text: $text, prompt: Text("Search fleet"))
        } else {
            content
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
