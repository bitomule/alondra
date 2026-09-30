import AlondraContracts
import Foundation
import ComposableArchitecture
import AlondraViews
import AlondraViewsDependency
import AlondraViewsUI

/// Per-window navigation and disclosure state. The service graph remains the sole
/// owner of snapshots, drafts and mutations.
@Reducer
public struct MacPresentationFeature {
    public init() {}
    public enum Selection: Hashable { case viewKey(String) }
    @ObservableState
    public struct State: Equatable {
        public init() {}
        public var selection: Selection?
        public var query = ""
        @Shared(.appStorage("tasksExpanded")) public var tasksExpanded = true
        @Shared(.appStorage("collapsedProjects")) public var collapsedProjectsRaw = ""
        @Shared(.appStorage("toggledTaskGroups")) public var toggledTaskGroupsRaw = ""
        public var collapsedProjects: Set<String> { Set(collapsedProjectsRaw.split(separator: "\n").map(String.init)) }
        public var toggledTaskGroups: Set<String> { Set(toggledTaskGroupsRaw.split(separator: "\n").map(String.init)) }
        public var smartMatches: Set<String>?
        public var smartStatus = ""
        public var searchRevision: UInt64 = 0
        public var sidebarDocument: ViewDocument?
        public var detailDocument: ViewDocument?
        public var selectedViewKey: String?
        public var viewError: String?
        public var actionFeature = ActionFeature.State(dataEpoch: "", client: .mac, lang: "en")
        public var actionPresented = false
        public var activeForm: AlondraViews.Form?
        public var contentTitle = ""
        public var contentBody = ""
        public var contentIsDiff = false
        public var contentLoading = false
        public var contentError: String?
        public var contentPresented = false
        public var contentImageData: Data?

        private var terms: [Substring] { query.lowercased().split(separator: " ") }
        public func projectExpanded(_ name: String) -> Bool { !query.isEmpty || !collapsedProjects.contains(name) }
        public func rowExpanded(_ id: String, defaultExpanded: Bool = true) -> Bool {
            !toggledTaskGroups.contains(id) ? defaultExpanded : !defaultExpanded
        }
        public func matches(_ text: String) -> Bool {
            let haystack = text.lowercased()
            return terms.allSatisfy { haystack.contains($0) }
        }
    }
    public enum Action {
        case select(Selection?)
        case queryChanged(String)
        case searchRequested
        case searchStarted(UInt64)
        case searchFinished(UInt64, ViewFetch?)
        case disappeared
        case tasksExpanded(Bool)
        case rowExpanded(String, defaultExpanded: Bool, expanded: Bool)
        case sidebarSectionExpanded(String, Bool)
        case appeared
        case viewsStale([String])
        case viewsReset
        case viewLoaded(String, ViewFetch)
        case viewFailed(String, String)
        case beginViewAction(AlondraViews.Action)
        case beginViewForm(AlondraViews.Form)
        case contentRequested(AlondraViews.ContentRef)
        case attachmentRequested(AlondraViews.Attachment)
        case contentLoaded(AlondraViews.ContentRef, String)
        case attachmentLoaded(AlondraViews.Attachment, Data)
        case contentFailed(String)
        case contentPresented(Bool)
        case actionPresented(Bool)
        case action(ActionFeature.Action)
    }
    @Dependency(\.continuousClock) var clock
    @Dependency(\.viewsClient) var viewsClient
    private enum CancelID { case search }
    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .beginViewAction(let action):
                guard let document = state.detailDocument ?? state.sidebarDocument else { return .none }
                let lang = Locale.preferredLanguages.first?.lowercased().hasPrefix("es") == true ? "es" : "en"
                state.actionFeature = ActionFeature.State(dataEpoch: document.dataEpoch, client: .mac, lang: lang)
                state.activeForm = nil
                state.actionPresented = true
                return .send(.action(.present(action)))
            case .beginViewForm(let form):
                guard let document = state.detailDocument ?? state.sidebarDocument else { return .none }
                let lang = Locale.preferredLanguages.first?.lowercased().hasPrefix("es") == true ? "es" : "en"
                state.actionFeature = ActionFeature.State(dataEpoch: document.dataEpoch, client: .mac, lang: lang)
                state.activeForm = form
                state.actionPresented = true
                let requiresAttachments = form.fields.contains { if case .attachments = $0.kind { true } else { false } }
                return .send(.action(.presentForm(form.submit, Dictionary(uniqueKeysWithValues: form.fields.map { ($0.id, $0.value) }), requiresAttachments: requiresAttachments)))
            case .contentRequested(let content):
                state.contentTitle = content.title
                state.contentIsDiff = content.kind == .diff
                state.contentBody = ""
                state.contentError = nil
                state.contentLoading = true
                state.contentPresented = true
                state.contentImageData = nil
                return .run { [viewsClient] send in
                    do { await send(.contentLoaded(content, try await viewsClient.fetchContent(content))) }
                    catch { await send(.contentFailed(String(localized: "Couldn’t load this from your Mac. Try again."))) }
                }
            case .attachmentRequested(let attachment):
                state.contentTitle = attachment.name
                state.contentIsDiff = false
                state.contentBody = ""
                state.contentError = nil
                state.contentLoading = true
                state.contentPresented = true
                state.contentImageData = nil
                return .run { [viewsClient] send in
                    do { await send(.attachmentLoaded(attachment, try await viewsClient.fetchAttachment(attachment))) }
                    catch { await send(.contentFailed(String(localized: "Couldn’t load this from your Mac. Try again."))) }
                }
            case let .contentLoaded(content, body):
                state.contentTitle = content.title
                state.contentIsDiff = content.kind == .diff
                state.contentBody = body
                state.contentLoading = false
                state.contentError = nil
                state.contentImageData = nil
            case let .attachmentLoaded(attachment, data):
                state.contentTitle = attachment.name
                state.contentLoading = false
                state.contentError = nil
                if attachment.kind == .image {
                    state.contentImageData = data
                } else if let text = String(data: data, encoding: .utf8) {
                    state.contentBody = text
                    state.contentImageData = nil
                } else {
                    state.contentError = String(localized: "This attachment can’t be previewed as text.")
                    state.contentImageData = nil
                }
            case .contentFailed(let message):
                state.contentLoading = false
                state.contentError = message
            case .contentPresented(let value):
                state.contentPresented = value
            case .actionPresented(let value):
                state.actionPresented = value
                if !value {
                    state.actionFeature.action = nil
                    state.activeForm = nil
                }
            case .action(let childAction):
                if case .response(let reply) = childAction, reply.outcome == .applied {
                    let keys = ["sidebar", state.selectedViewKey].compactMap { $0 }
                    return .merge(keys.map { key in
                        let known = key == "sidebar" ? state.sidebarDocument?.version : state.detailDocument?.version
                        return loadView(key, known: known)
                    })
                }
            case .select(let selection):
                guard let key = viewKey(for: selection) else {
                    state.selection = nil
                    state.selectedViewKey = nil
                    state.detailDocument = nil
                    return openVisibleViews(&state)
                }
                state.selection = .viewKey(key)
                state.selectedViewKey = key
                state.detailDocument = nil
                return loadAndOpenView(key, visibleKeys: visibleViewKeys(state))
            case .appeared:
                return loadAndOpenView("sidebar", visibleKeys: visibleViewKeys(state))
            case .viewsStale(let keys):
                let visible = Set(["sidebar", state.selectedViewKey].compactMap { $0 })
                return .merge(keys.filter(visible.contains).map { key in
                    let known = key == "sidebar" ? state.sidebarDocument?.version : state.detailDocument?.version
                    return loadView(key, known: known)
                })
            case .viewsReset:
                state.sidebarDocument = nil
                state.detailDocument = nil
                return .merge(loadView("sidebar"), state.selectedViewKey.map { loadView($0) } ?? .none)
            case let .viewLoaded(key, fetch):
                state.viewError = nil
                if case let .document(document) = fetch {
                    if key == "sidebar" {
                        state.sidebarDocument = document
                        let links = document.sections.flatMap(\.items).flatMap { item -> [String] in
                            guard case let .row(row) = item else { return [] }
                            return viewLinks(in: row)
                        }
                        if case let .viewKey(current)? = state.selection, !links.contains(current) {
                            let next = links.first
                            state.selection = next.map(Selection.viewKey)
                            state.selectedViewKey = next
                            state.detailDocument = nil
                            if let next { return loadAndOpenView(next, visibleKeys: visibleViewKeys(state)) }
                            return openVisibleViews(&state)
                        } else if state.selection == nil, let first = links.first {
                            state.selection = .viewKey(first)
                            state.selectedViewKey = first
                            return loadAndOpenView(first, visibleKeys: visibleViewKeys(state))
                        }
                    }
                    if key == state.selectedViewKey { state.detailDocument = document }
                }
            case let .viewFailed(_, message): state.viewError = message
            case .queryChanged(let query):
                state.query = query
                state.smartMatches = nil
                state.smartStatus = ""
                state.searchRevision &+= 1
                return .cancel(id: CancelID.search)
            case .searchRequested:
                let query = state.query.trimmingCharacters(in: .whitespaces)
                guard query.count >= 3 else { return .none }
                let revision = state.searchRevision
                return .run { [viewsClient] send in
                    try await clock.sleep(for: .milliseconds(1200))
                    await send(.searchStarted(revision))
                    let result = try? await viewsClient.load(ViewKey.search(query), nil)
                    try Task.checkCancellation()
                    await send(.searchFinished(revision, result))
                }.cancellable(id: CancelID.search, cancelInFlight: true)
            case .searchStarted(let revision):
                guard revision == state.searchRevision else { return .none }
                state.smartStatus = String(localized: "Searching the conversations…")
            case .searchFinished(let revision, let result):
                guard revision == state.searchRevision else { return .none }
                guard case let .document(document)? = result else {
                    state.smartMatches = nil
                    state.smartStatus = ""
                    return .none
                }
                let matches = viewRowIDs(document.sections.flatMap(\.items))
                state.smartMatches = Set(matches)
                state.smartStatus = matches.isEmpty ? String(localized: "No results") : String(localized: "Search results")
            case .disappeared:
                state.searchRevision &+= 1
                return .merge(.cancel(id: CancelID.search), .run { _ in await viewsClient.closeScreen("mac-presentation") })
            case .tasksExpanded(let expanded): state.$tasksExpanded.withLock { $0 = expanded }
            case .rowExpanded(let key, let defaultExpanded, let expanded):
                state.$toggledTaskGroupsRaw.withLock { raw in
                    var toggled = Set(raw.split(separator: "\n").map(String.init))
                    if expanded == defaultExpanded { toggled.remove(key) } else { toggled.insert(key) }
                    raw = toggled.sorted().joined(separator: "\n")
                }
            case .sidebarSectionExpanded(let id, let expanded):
                if id == "tasks" { state.$tasksExpanded.withLock { $0 = expanded } }
                else if id.hasPrefix("project:") {
                    let name = String(id.dropFirst("project:".count))
                    state.$collapsedProjectsRaw.withLock { raw in
                        var collapsed = Set(raw.split(separator: "\n").map(String.init))
                        if expanded { collapsed.remove(name) } else { collapsed.insert(name) }
                        raw = collapsed.sorted().joined(separator: "\n")
                    }
                }
            }
            return .none
        }
        Scope(state: \.actionFeature, action: \.action) { ActionFeature() }
    }

    private func loadView(_ key: String, known: String? = nil) -> Effect<Action> {
        .run { [viewsClient] send in
            do {
                await send(.viewLoaded(key, try await viewsClient.load(ViewKey(rawValue: key), known)))
            } catch { await send(.viewFailed(key, String(localized: "Couldn’t load this from your Mac. Try again."))) }
        }
    }

    private func loadAndOpenView(_ key: String, visibleKeys: Set<ViewKey>) -> Effect<Action> {
        .run { [viewsClient] send in
            do {
                let result = try await viewsClient.load(ViewKey(rawValue: key), nil)
                await viewsClient.openScreen("mac-presentation", visibleKeys)
                await send(.viewLoaded(key, result))
            } catch { await send(.viewFailed(key, String(localized: "Couldn’t load this from your Mac. Try again."))) }
        }
    }

    private func visibleViewKeys(_ state: State) -> Set<ViewKey> {
        Set(["sidebar", state.selectedViewKey].compactMap { $0 }.map { ViewKey(rawValue: $0) })
    }

    private func viewKey(for selection: Selection?) -> String? {
        guard let selection else { return nil }
        switch selection {
        case .viewKey(let key): return key
        }
    }

    private func openVisibleViews(_ state: inout State) -> Effect<Action> {
        let keys = visibleViewKeys(state)
        return .run { [viewsClient] _ in await viewsClient.openScreen("mac-presentation", keys) }
    }
}

private func viewLinks(in row: Row) -> [String] {
    let own: [String]
    if case let .view(key)? = row.link { own = [key] } else { own = [] }
    return own + row.children.flatMap(viewLinks(in:))
}

private func viewRowIDs(_ items: [ViewItem]) -> [String] {
    items.flatMap { item -> [String] in
        switch item {
        case let .row(row): return [row.id] + row.children.flatMap { viewRowIDs([.row($0)]) }
        case let .card(card): return [card.id] + card.children.flatMap { viewRowIDs([.card($0)]) }
        default: return []
        }
    }
}
