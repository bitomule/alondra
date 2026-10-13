import AlondraContract
import Foundation
import ComposableArchitecture
import AlondraFeed

/// Per-window navigation and disclosure state. The service graph remains the sole
/// owner of snapshots, drafts and mutations.
/// The rows of a folded section, fetched when it is opened, and the page after them if there is one.
public struct SectionPage: Equatable, Sendable {
    public var items: [ViewItem]
    public var more: More?

    public init(items: [ViewItem], more: More?) {
        self.items = items
        self.more = more
    }
}

@Reducer
public struct MacPresentationFeature: Sendable {
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
        /// Folded sections of the detail view the user opened, and the rows they were sent when opened.
        public var openSections: Set<String> = []
        public var sectionPages: [String: SectionPage] = [:]
        public var selectedViewKey: String?
        public var viewError: String?
        public var actionFeature = ActionFeature.State(dataEpoch: "", client: .mac, lang: "en")
        public var actionPresented = false
        public var actionTarget: String?
        public var optimistic = Optimistic()
        /// Requests that went out without an answer, by action: pressing the same again resends them.
        public var unsure: [String: ActionRequest] = [:]
        /// Why the daemon did not take a card's answer, by card, shown when the card is listed again.
        public var returnReasons: [String: String] = [:]
        /// Cards whose answer the daemon did not take, by that command, until its reason arrives.
        public var returning: [String: String] = [:]
        /// Cards answered on this Mac whose answer has not reached the daemon yet, also after a relaunch.
        public var undelivered: Set<String> = []
        /// Whether the sidebar leaves a row out: answered here and not yet given back by the daemon.
        public func hides(_ id: String) -> Bool { optimistic.hides(id) || undelivered.contains(id) }
        public var activeForm: AlondraContract.Form?
        public var pickerFieldID: String?
        public var pickerDocument: ViewDocument?
        public var pickerQuery = ""
        public var pickerLoading = false
        public var pickerError: String?
        public var contentTitle = ""
        public var contentBody = ""
        public var contentIsDiff = false
        public var contentLoading = false
        public var contentError: String?
        public var contentPresented = false
        public var contentImageData: Data?
        public var contentDigest: String?
        public var diffComments: [String: ActionComment] = [:]

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
        case sectionToggled(ViewSection)
        case sectionMore(String, key: String)
        case sectionLoaded(String, ViewFetch, appending: Bool)
        /// The selected card no longer waits for the user: it is never shown, and the window says why.
        case settled(String)
        case beginViewAction(AlondraContract.Action, target: String? = nil)
        case sendReply(AlondraContract.Action, text: String)
        case beginViewForm(AlondraContract.Form)
        case reloadForm(String, JSONValue)
        case pickerOpened(String, String), pickerQueryChanged(String), pickerLoaded(ViewFetch), pickerFailed(String)
        case pickerSelected(String), pickerClosed
        case diffCommentsChanged([String: ActionComment])
        case contentRequested(AlondraContract.ContentRef)
        case attachmentRequested(AlondraContract.Attachment)
        case contentLoaded(AlondraContract.ContentRef, String)
        case attachmentLoaded(AlondraContract.Attachment, Data)
        case contentFailed(String)
        case contentPresented(Bool)
        case actionPresented(Bool)
        case action(ActionFeature.Action)
        case undeliveredChanged(Set<String>)
    }
    @Dependency(\.continuousClock) var clock
    @Dependency(\.viewsClient) var viewsClient
    private enum CancelID: Hashable { case search, picker, undelivered }
    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case let .beginViewAction(action, target):
                guard let document = state.detailDocument ?? state.sidebarDocument else { return .none }
                let lang = Locale.preferredLanguages.first?.lowercased().hasPrefix("es") == true ? "es" : "en"
                state.actionFeature = ActionFeature.State(dataEpoch: document.dataEpoch, client: .mac, lang: lang)
                state.actionFeature.unsure = action.id.flatMap { state.unsure[$0] }
                state.activeForm = nil
                state.actionTarget = target ?? state.selectedViewKey.flatMap(Self.cardID)
                state.actionPresented = !action.sendsWithoutAsking
                if action.comments?.kind == .diffLines {
                    let comments = state.diffComments.values.sorted { $0.anchor < $1.anchor }
                    return .concatenate(.send(.action(.present(action))), .send(.action(.commentsChanged(comments))))
                }
                if action.sendsWithoutAsking {
                    return .concatenate(.send(.action(.present(action))), .send(.action(.submit)))
                }
                return .send(.action(.present(action)))
            case let .sendReply(action, text):
                guard let document = state.detailDocument ?? state.sidebarDocument else { return .none }
                let lang = Locale.preferredLanguages.first?.lowercased().hasPrefix("es") == true ? "es" : "en"
                state.actionFeature = ActionFeature.State(dataEpoch: document.dataEpoch, client: .mac, lang: lang)
                state.actionFeature.unsure = action.id.flatMap { state.unsure[$0] }
                state.activeForm = nil
                state.actionTarget = state.selectedViewKey.flatMap(Self.cardID)
                state.actionPresented = false
                return .concatenate(.send(.action(.present(action))), .send(.action(.textChanged(text))), .send(.action(.submit)))
            case .beginViewForm(let form):
                guard let document = state.detailDocument ?? state.sidebarDocument else { return .none }
                let lang = Locale.preferredLanguages.first?.lowercased().hasPrefix("es") == true ? "es" : "en"
                state.actionFeature = ActionFeature.State(dataEpoch: document.dataEpoch, client: .mac, lang: lang)
                state.activeForm = form
                state.actionTarget = nil
                state.actionPresented = true
                let requiresAttachments = form.fields.contains { if case .attachments = $0.kind { true } else { false } }
                return .send(.action(.presentForm(form.submit, Dictionary(uniqueKeysWithValues: form.fields.map { ($0.id, $0.value) }), requiresAttachments: requiresAttachments)))
            case .reloadForm(let fieldID, let value):
                guard fieldID == "project", case let .string(project) = value,
                      let key = state.selectedViewKey else { return .none }
                let next: ViewKey
                if key.hasPrefix("new_request") { next = .newRequest(project: project) }
                else if key.hasPrefix("schedule:new") { next = .newSchedule(project: project) }
                else { return .none }
                guard next.rawValue != key else { return .none }
                state.selectedViewKey = next.rawValue
                return loadAndOpenView(next.rawValue, visibleKeys: visibleViewKeys(state))
            case .pickerOpened(let fieldID, let source):
                guard source == "issue_picker" else { return .none }
                state.pickerFieldID = fieldID
                state.pickerDocument = nil
                state.pickerQuery = ""
                state.pickerLoading = true
                state.pickerError = nil
                return loadPicker(query: "")
            case .pickerQueryChanged(let query):
                guard state.pickerFieldID != nil else { return .none }
                state.pickerQuery = query
                state.pickerLoading = true
                return .run { [clock, viewsClient] send in
                    do {
                        try await clock.sleep(for: .milliseconds(250))
                        await send(.pickerLoaded(try await viewsClient.load(.issuePicker(query: query), nil)))
                    } catch is CancellationError { return }
                    catch { await send(.pickerFailed(String(localized: "Couldn’t load this from your Mac. Try again."))) }
                }.cancellable(id: CancelID.picker, cancelInFlight: true)
            case .pickerLoaded(.document(let document)):
                guard state.pickerFieldID != nil else { return .none }
                state.pickerDocument = document
                state.pickerLoading = false
                state.pickerError = nil
            case .pickerLoaded(.unchanged): state.pickerLoading = false
            case .pickerFailed(let message):
                state.pickerLoading = false
                state.pickerError = message
            case .pickerSelected(let value):
                guard let fieldID = state.pickerFieldID else { return .none }
                state.actionFeature.fields[fieldID] = .string(value)
                state.pickerFieldID = nil
                state.pickerDocument = nil
                return .cancel(id: CancelID.picker)
            case .pickerClosed:
                state.pickerFieldID = nil
                state.pickerDocument = nil
                return .cancel(id: CancelID.picker)
            case .diffCommentsChanged(let comments): state.diffComments = comments
            case .contentRequested(let content):
                let identity = content.digest ?? content.fetch
                if state.contentDigest != identity { state.diffComments = [:] }
                state.contentDigest = identity
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
                    state.actionTarget = nil
                }
            case .action(.sent(let request)):
                guard request.dataEpoch == state.actionFeature.dataEpoch, let target = state.actionTarget else { return .none }
                state.actionTarget = nil
                guard request.resolvesItsCard else { return .none }
                state.optimistic.begin(target, request, dataEpoch: state.actionFeature.dataEpoch)
                state.returnReasons[target] = nil
                state.actionPresented = false
                return leaveCard(target, &state)
            case .action(.settled(let request, let applied)):
                guard request.dataEpoch == state.actionFeature.dataEpoch else { return .none }
                let shown = state.sidebarDocument?.version
                guard let returned = state.optimistic.settle(request, applied: applied, shown: shown) else { return .none }
                state.optimistic.hold(returned, shown: shown, dataEpoch: request.dataEpoch)
                state.returning[request.commandId] = returned
                return loadView("sidebar", known: shown)
            case .action(.undelivered(let request, let failure)):
                guard request.dataEpoch == state.actionFeature.dataEpoch else { return .none }
                state.unsure[request.actionId] = failure == .unsure ? request : nil
                if let card = state.returning.removeValue(forKey: request.commandId) { state.returnReasons[card] = failure.message }
            case .action(.answered(let request, let reply)):
                guard request.dataEpoch == state.actionFeature.dataEpoch else { return .none }
                if state.unsure[request.actionId]?.commandId == request.commandId { state.unsure[request.actionId] = nil }
                guard let card = state.returning.removeValue(forKey: request.commandId) else { return .none }
                switch OutcomeStyle(reply) {
                case .failure(let reason): state.returnReasons[card] = reason
                case .ask where state.actionFeature.pending?.commandId == request.commandId:
                    state.optimistic.release(card)
                    state.actionTarget = card
                    return bringBack(card, &state)
                case .ask:
                    state.optimistic.release(card)
                    return bringBack(card, &state)
                case .success: break
                }
            case .action(.response(let reply)) where reply.outcome == .changed:
                state.actionPresented = true
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
                state.openSections = []
                state.sectionPages = [:]
                return loadAndOpenView(key, visibleKeys: visibleViewKeys(state))
            case .sectionToggled(let section):
                guard state.openSections.insert(section.id).inserted else {
                    state.openSections.remove(section.id)
                    return .none
                }
                guard state.sectionPages[section.id] == nil, section.items.isEmpty, let more = section.more else { return .none }
                return loadSection(section.id, key: more.key, appending: false)
            case let .sectionMore(id, key):
                return loadSection(id, key: key, appending: true)
            case let .sectionLoaded(id, fetch, appending):
                guard case let .document(document) = fetch,
                      let loaded = document.sections.first(where: { $0.id == id }) else { return .none }
                let before = appending ? state.sectionPages[id]?.items ?? [] : []
                state.sectionPages[id] = SectionPage(items: before + loaded.items, more: loaded.more)
            case .appeared:
                return .merge(loadAndOpenView("sidebar", visibleKeys: visibleViewKeys(state)), watchUndelivered())
            case .undeliveredChanged(let cards):
                state.undelivered = cards
            case .viewsStale(let keys):
                let visible = Set(["sidebar", state.selectedViewKey].compactMap { $0 })
                return .merge(keys.filter(visible.contains).map { key in
                    let known = key == "sidebar" ? state.sidebarDocument?.version : state.detailDocument?.version
                    return loadView(key, known: known)
                })
            case .viewsReset:
                state.returnReasons = [:]
                state.sidebarDocument = nil
                state.detailDocument = nil
                return .merge(loadView("sidebar"), state.selectedViewKey.map { loadView($0) } ?? .none)
            case let .viewLoaded(key, fetch):
                if case let .document(document) = fetch {
                    if key != "sidebar", let sidebar = state.sidebarDocument,
                       document.dataEpoch != sidebar.dataEpoch { return .none }
                    state.viewError = nil
                    state.actionFeature.dataEpoch = document.dataEpoch
                    if key == "sidebar", let detail = state.detailDocument, detail.dataEpoch != document.dataEpoch {
                        state.detailDocument = nil
                        state.activeForm = nil
                        state.actionFeature.action = nil
                        state.actionPresented = false
                        state.actionTarget = nil
                    }
                    if key == "sidebar" {
                        let previous = state.sidebarDocument
                        if previous?.version != document.version { state.returnReasons = [:] }
                        state.sidebarDocument = document
                        state.optimistic.reconcile(document.version, dataEpoch: document.dataEpoch)
                        let links = Self.links(in: document)
                        if case let .viewKey(current)? = state.selection, !links.contains(current),
                           Self.links(in: previous).contains(current) {
                            let next = Self.nextKey(after: current, in: previous, available: Set(links)) ?? links.first
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
                    if key == state.selectedViewKey, let why = document.settledNotice {
                        return closeSettled(key, why: why, &state)
                    }
                    if key == state.selectedViewKey {
                        let reopen = state.detailDocument?.version == document.version ? [] : document.sections.filter {
                            state.openSections.contains($0.id) && state.sectionPages[$0.id] != nil
                        }
                        state.detailDocument = document
                        if state.activeForm != nil,
                           let updated = document.sections.lazy.flatMap(\.items).compactMap({ item -> AlondraContract.Form? in
                               if case let .form(form) = item { form } else { nil }
                           }).first {
                            state.activeForm = updated
                            state.actionFeature.action = updated.submit
                            state.actionFeature.fields = Dictionary(uniqueKeysWithValues: updated.fields.map {
                                ($0.id, state.actionFeature.fields[$0.id] ?? $0.value)
                            })
                        }
                        return .merge(reopen.compactMap { section in
                            section.more.map { loadSection(section.id, key: $0.key, appending: false) }
                        })
                    }
                } else { state.viewError = nil }
            case let .viewFailed(_, message): state.viewError = message
            case .settled: break
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

    /// The daemon asks about an answer again: its card is shown, unless the person is already
    /// answering another one.
    private func bringBack(_ id: String, _ state: inout State) -> Effect<Action> {
        guard !state.actionPresented || state.actionTarget == id,
              let key = Self.waitingEntries(in: state.sidebarDocument).first(where: { $0.id == id })?.key,
              key != state.selectedViewKey else { return .none }
        state.selection = .viewKey(key)
        state.selectedViewKey = key
        state.detailDocument = nil
        return loadAndOpenView(key, visibleKeys: visibleViewKeys(state))
    }

    private func watchUndelivered() -> Effect<Action> {
        .run { [viewsClient] send in
            for await cards in viewsClient.undeliveredCards() { await send(.undeliveredChanged(cards)) }
        }
        .cancellable(id: CancelID.undelivered, cancelInFlight: true)
    }

    /// A settled card leaves the detail for the next waiting one, or for nothing.
    private func closeSettled(_ key: String, why: String, _ state: inout State) -> Effect<Action> {
        let available = Set(Self.waitingEntries(in: state.sidebarDocument).map(\.key)).subtracting([key])
        let next = Self.nextKey(after: key, in: state.sidebarDocument, available: available)
        state.selection = next.map(Selection.viewKey)
        state.selectedViewKey = next
        state.detailDocument = nil
        let move = next.map { loadAndOpenView($0, visibleKeys: visibleViewKeys(state)) } ?? openVisibleViews(&state)
        return .merge(move, .send(.settled(why)))
    }

    private func leaveCard(_ id: String, _ state: inout State) -> Effect<Action> {
        guard let key = state.selectedViewKey, Self.cardID(forViewKey: key) == id else { return .none }
        let hidden = state
        let available = Set(Self.waitingEntries(in: state.sidebarDocument).filter { !hidden.hides($0.id) }.map(\.key))
        guard let next = Self.nextKey(after: key, in: state.sidebarDocument, available: available) else { return .none }
        state.selection = .viewKey(next)
        state.selectedViewKey = next
        state.detailDocument = nil
        return loadAndOpenView(next, visibleKeys: visibleViewKeys(state))
    }

    public static func cardID(forViewKey key: String) -> String? {
        for prefix in ["card:", "pull_request:"] where key.hasPrefix(prefix) {
            return String(key.dropFirst(prefix.count))
        }
        return nil
    }

    /// What waits for the person, in the order the sidebar lists it.
    static func waitingEntries(in document: ViewDocument?) -> [(id: String, key: String)] {
        guard let document else { return [] }
        return document.sections.filter { $0.id == "waiting" }
            .flatMap(\.items)
            .compactMap { item in
                guard case let .row(row) = item, case let .view(key)? = row.link else { return nil }
                return (row.id, key)
            }
    }

    /// The card that takes the place of `key` once it is gone: the one after it, else the last before it.
    public static func nextKey(after key: String, in document: ViewDocument?, available: Set<String>) -> String? {
        let entries = Self.waitingEntries(in: document)
        guard let index = entries.firstIndex(where: { $0.key == key }) else { return nil }
        let remaining = entries.enumerated().filter { $0.offset != index && available.contains($0.element.key) }
        return (remaining.first { $0.offset > index } ?? remaining.last)?.element.key
    }

    static func links(in document: ViewDocument?) -> [String] {
        document?.sections.flatMap(\.items).flatMap { item -> [String] in
            guard case let .row(row) = item else { return [] }
            return viewLinks(in: row)
        } ?? []
    }

    private func loadView(_ key: String, known: String? = nil) -> Effect<Action> {
        .run { [viewsClient] send in
            do {
                await send(.viewLoaded(key, try await viewsClient.load(ViewKey(rawValue: key), known)))
            } catch { await send(.viewFailed(key, String(localized: "Couldn’t load this from your Mac. Try again."))) }
        }
    }

    private func loadSection(_ id: String, key: String, appending: Bool) -> Effect<Action> {
        .run { [viewsClient] send in
            do {
                await send(.sectionLoaded(id, try await viewsClient.load(ViewKey(rawValue: key), nil), appending: appending))
            } catch { await send(.viewFailed(key, String(localized: "Couldn’t load this from your Mac. Try again."))) }
        }
    }

    private func loadPicker(query: String) -> Effect<Action> {
        .run { [viewsClient] send in
            do { await send(.pickerLoaded(try await viewsClient.load(.issuePicker(query: query), nil))) }
            catch { await send(.pickerFailed(String(localized: "Couldn’t load this from your Mac. Try again."))) }
        }.cancellable(id: CancelID.picker, cancelInFlight: true)
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
        case let .card(card): return [card.id]
        default: return []
        }
    }
}

extension AlondraContract.Action {
    /// A button that needs nothing more from the person: no text, no choice, no confirmation.
    var sendsWithoutAsking: Bool {
        guard enabled, id != nil, link == nil, confirm == nil, !attachments, comments == nil,
              presentation == .button, options.isEmpty else { return false }
        if case .none = input { return true }
        return false
    }
}
