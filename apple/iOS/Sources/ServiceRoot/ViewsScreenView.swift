import AlondraViews
import AlondraViewsDependency
import AlondraViewsUI
import AlondraWorkspaceFeature
import AlondraShared
import ComposableArchitecture
import SwiftUI
import UIKit

struct ViewsScreenView: View {
    @Bindable var store: StoreOf<ViewsScreenFeature>
    @Environment(\.openURL) private var openURL
    @State private var diffComments: [String: ActionComment] = [:]

    private var isTasksScreen: Bool { store.key.rawValue == "tasks" || store.key.rawValue.hasPrefix("tasks?") }
    private var isNewRequestScreen: Bool { store.key.rawValue == "new_request" || store.key.rawValue.hasPrefix("new_request?") }
    private var documentCard: AlondraViews.Card? {
        guard store.key.rawValue.hasPrefix("card:") else { return nil }
        return store.document?.sections.lazy.flatMap(\.items).compactMap { item in
            if case let .card(card) = item { card } else { nil }
        }.first
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

    var body: some View {
        Group {
            if let card = documentCard {
                ServiceWireDocumentView(card: card, onAction: { action, comments, text in
                    store.send(.action(.present(action)))
                    store.send(.action(.commentsChanged(comments)))
                    store.send(.action(.textChanged(text)))
                }, onLink: follow, onAttachment: { store.send(.fetchAttachment($0)) })
                .id(card.id)
            } else if let document = store.document {
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
                        ForEach(document.sections, id: \.id) { section in
                            sectionContent(section)
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
                        if let empty = document.empty { NoticeView(notice: empty) }
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
                .toolbar {
                    if store.parentKey != nil {
                        ToolbarItem(placement: .topBarLeading) {
                            Button("Back", systemImage: "chevron.left") { store.send(.back) }
                        }
                    }
                    ToolbarItem(placement: .topBarTrailing) {
                        Button("Refresh", systemImage: "arrow.clockwise") { store.send(.refresh) }
                    }
                }
            } else if store.loading {
                ProgressView().tint(Palette.dawn).frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(DawnBackground(glow: screenGlow))
            } else {
                ContentUnavailableView(store.error ?? "Unable to load this screen", systemImage: "exclamationmark.triangle")
                    .background(DawnBackground(glow: screenGlow))
            }
        }
        .task { await store.send(.appeared).finish() }
        .task(id: store.document?.version) {
            guard store.key.rawValue.hasPrefix("pull_request:"), let document = store.document else { return }
            guard let diff = document.sections.lazy.flatMap(\.items).compactMap({ item -> ContentRef? in
                if case let .contentRef(reference) = item, reference.kind == .diff { reference } else { nil }
            }).first else { return }
            if store.contentReference?.digest != diff.digest {
                diffComments = [:]
                store.send(.fetchContent(diff))
            }
        }
        .onDisappear { store.send(.disappeared) }
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
        .sheet(isPresented: Binding(get: { store.action.action != nil }, set: { if !$0 { store.send(.action(.dismiss)) } })) {
            ActionInputView(store: store.scope(state: \.action, action: \.action), form: store.form,
                            showCommentEditor: documentCard == nil && !store.key.rawValue.hasPrefix("pull_request:"),
                            onReload: { field, value in store.send(.reloadForm(field.id, value)) },
                            onPicker: { field, source in store.send(.pickerOpened(field.id, source)) })
                .padding()
                .presentationDetents([.medium, .large])
                .sheet(isPresented: Binding(get: { store.pickerFieldID != nil }, set: { if !$0 { store.send(.pickerClosed) } })) {
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
                .safeAreaInset(edge: .bottom) {
                    Button("Continue") { store.send(.action(.submit)) }
                        .buttonStyle(.borderedProminent).tint(Palette.dawn).padding()
                }
        }
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

    private func follow(_ link: AlondraViews.Link) {
        switch link {
        case .view(let key): store.send(.open(ViewKey(rawValue: key)))
        case .url(let raw):
            guard let url = URL(string: raw), url.scheme?.lowercased() == "https",
                  let host = url.host?.lowercased(), host == "github.com" || host.hasSuffix(".github.com") else { return }
            openURL(url)
        case .local(let local):
            if case let .attach(session) = local { store.send(.open(.card(session))) }
        }
    }

    private func present(_ action: AlondraViews.Action) {
        store.send(.action(.present(action)))
        if action.comments?.kind == .diffLines {
            store.send(.action(.commentsChanged(diffComments.values.sorted { $0.anchor < $1.anchor })))
        }
    }

    @ViewBuilder private func sectionContent(_ section: ViewSection) -> some View {
        if isTasksScreen, ["active", "finished", "archived"].contains(section.id) {
            if !section.collapsed {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(alignment: .firstTextBaseline) {
                        if let title = section.title {
                            Text(title).font(.display(.title3, weight: .bold)).foregroundStyle(Palette.ink)
                        }
                        Spacer()
                        if let count = section.count { Text("\(count)").font(.caption.monospacedDigit()).foregroundStyle(.secondary) }
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
                        Button("Show \(more.remaining) more") { store.send(.open(ViewKey(rawValue: more.key))) }
                            .font(.footnote.weight(.semibold)).foregroundStyle(Palette.dawn)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.top, 24)
            }
        } else if store.key.rawValue == "fleet",
                  section.id == "nodes" || section.id == "orchestrator" || section.id.hasPrefix("project:") {
            VStack(alignment: .leading, spacing: 8) {
                if let title = section.title {
                    Text(title).font(.display(.title3, weight: .bold)).foregroundStyle(Palette.ink)
                }
                let rows = section.items.compactMap { item -> AlondraViews.Row? in
                    if case let .row(row) = item { row } else { nil }
                }
                ForEach(rows, id: \.id) { row in FleetWireRow(row: row, onLink: follow) }
            }
        } else {
            SectionView(section: section, onAction: present,
                        onLink: follow, onMore: { store.send(.open(ViewKey(rawValue: $0))) },
                        onForm: presentForm, onContent: { store.send(.fetchContent($0)) },
                        onAttachment: { store.send(.fetchAttachment($0)) })
        }
    }

    private func presentForm(_ form: WireForm) {
        store.send(.presentForm(form))
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
            .background(Palette.card.opacity(0.85), in: .rect(cornerRadius: 16, style: .continuous))
        }
        .buttonStyle(.plain)
    }

    private var fleetTint: Color {
        switch row.stateKey {
        case "working", "running": Palette.sky
        case "offline", "disconnected": Palette.rose
        default: row.icon == .orchestrator ? Palette.ember : Palette.sage
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
        switch row.stateKey?.lowercased() {
        case "running": Palette.sky
        case "needs_you", "in_review": Palette.dawn
        case "done", "closing": Palette.sage
        case "cancelled", "waiting_dependency": Palette.slate
        default: Palette.slate
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
        switch row.stateKey?.lowercased() {
        case "running": "hammer.fill"
        case "needs_you": "hand.raised.fill"
        case "in_review": "arrow.triangle.pull"
        case "done", "closing": "checkmark"
        case "waiting_dependency": "link"
        case "cancelled": "xmark"
        default: "circle.dashed"
        }
    }
}
