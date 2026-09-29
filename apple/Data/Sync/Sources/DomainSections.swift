import AlondraContracts
import Foundation

/// The local copy of the sectioned reads (docs/domain-v2-read-endpoints.md §6). A section is
/// read again only when its token moved; interactions are read one by one, only those an inbox
/// entry or an open task points at, and again only when the inbox says their revision moved.
struct DomainSections: Sendable {
    static let pageLimit = 1_000
    static let sectionAttempts = 3

    private(set) var epoch: DomainV2.DataEpoch?
    private(set) var revision: DomainV2.Revision = 0
    private var tokens: [DomainV2.Section: String] = [:]
    private var inbox: [DomainV2.InboxItem] = []
    private var tasks: [DomainV2.Task] = []
    private var requests: [DomainV2.Request] = []
    private var agents: [DomainV2.Agent] = []
    private var pullRequests: [DomainV2.PullRequest] = []
    private var documents: [DomainV2.DocumentBundle] = []
    private var mergeAuthorizations: [DomainV2.MergeAuthorization] = []
    private var stackReadiness: [DomainV2.StackReadiness] = []
    private var supersededAcceptances: [DomainV2.SupersededAcceptance] = []
    private var interactions: [DomainV2.InteractionId: DomainV2.Interaction] = [:]

    static let synced: [DomainV2.Section] = [
        .inbox, .tasks, .requests, .agents, .pullRequests, .documents,
        .mergeAuthorizations, .stackReadiness, .supersededAcceptances,
    ]

    /// Every synced section and every interaction: what a screen that shows everything needs.
    static let everything: Set<DomainV2.Section> = Set(synced).union([.interactions])

    /// Brings the sections in `demand` to the summary's tokens and returns the assembled view;
    /// the others stay as they were, empty until a screen asks for them. Stale sections are read
    /// concurrently; each one is still paged at a single token, and nothing is applied unless
    /// every read succeeded. `.interactions` in `demand` means every interaction the inbox and
    /// the open tasks point at; without it only the first pending card's are read.
    mutating func refresh(to summary: DomainV2.Summary, from transport: any DomainSyncTransport,
                          demand: Set<DomainV2.Section> = DomainSections.everything) async throws -> DomainV2.DomainProjection {
        if epoch != summary.dataEpoch { self = DomainSections() }
        guard summary.revision >= revision else {
            throw DomainV2.DomainError(code: "incompatible_protocol", message: "Domain revision moved backwards within one epoch",
                                       currentRevision: summary.revision, subject: nil)
        }
        epoch = summary.dataEpoch
        let stale = Self.synced.filter { section in
            guard demand.contains(section) else { return false }
            let token = summary.sections[section.rawValue]?.revision
            return token == nil || tokens[section] != token
        }
        let dataEpoch = summary.dataEpoch
        let reads = try await withThrowingTaskGroup(of: SectionRead.self) { group in
            for section in stale {
                group.addTask { try await Self.read(section, epoch: dataEpoch, from: transport) }
            }
            var reads: [SectionRead] = []
            for try await read in group { reads.append(read) }
            return reads
        }
        reads.forEach { apply($0) }
        try await readInteractions(epoch: summary.dataEpoch, all: demand.contains(.interactions), from: transport)
        revision = summary.revision
        return projection(summary)
    }

    private mutating func apply(_ read: SectionRead) {
        switch read {
        case .inbox(let rows, let token): (inbox, tokens[.inbox]) = (rows, token)
        case .tasks(let rows, let token): (tasks, tokens[.tasks]) = (rows, token)
        case .requests(let rows, let token): (requests, tokens[.requests]) = (rows, token)
        case .agents(let rows, let token): (agents, tokens[.agents]) = (rows, token)
        case .pullRequests(let rows, let token): (pullRequests, tokens[.pullRequests]) = (rows, token)
        case .documents(let rows, let token): (documents, tokens[.documents]) = (rows, token)
        case .mergeAuthorizations(let rows, let token): (mergeAuthorizations, tokens[.mergeAuthorizations]) = (rows, token)
        case .stackReadiness(let rows, let token): (stackReadiness, tokens[.stackReadiness]) = (rows, token)
        case .supersededAcceptances(let rows, let token): (supersededAcceptances, tokens[.supersededAcceptances]) = (rows, token)
        case .none: break
        }
    }

    private static func read(_ section: DomainV2.Section, epoch: DomainV2.DataEpoch,
                             from transport: any DomainSyncTransport) async throws -> SectionRead {
        switch section {
        case .inbox:
            let (rows, token) = try await all(section, DomainV2.InboxItem.self, epoch, transport)
            return .inbox(rows, token)
        case .tasks:
            let (rows, token) = try await all(section, DomainV2.Task.self, epoch, transport)
            return .tasks(rows, token)
        case .requests:
            let (rows, token) = try await all(section, DomainV2.Request.self, epoch, transport) { id in
                try await transport.domainRequest(.init(rawValue: id)).item
            }
            return .requests(rows, token)
        case .agents:
            let (rows, token) = try await all(section, DomainV2.Agent.self, epoch, transport)
            return .agents(rows, token)
        case .pullRequests:
            let (rows, token) = try await all(section, DomainV2.PullRequest.self, epoch, transport)
            return .pullRequests(rows, token)
        case .documents:
            let (rows, token) = try await all(section, DomainV2.DocumentBundle.self, epoch, transport)
            return .documents(rows, token)
        case .mergeAuthorizations:
            let (rows, token) = try await all(section, DomainV2.MergeAuthorization.self, epoch, transport)
            return .mergeAuthorizations(rows, token)
        case .stackReadiness:
            let (rows, token) = try await all(section, DomainV2.StackReadiness.self, epoch, transport)
            return .stackReadiness(rows, token)
        case .supersededAcceptances:
            let (rows, token) = try await all(section, DomainV2.SupersededAcceptance.self, epoch, transport)
            return .supersededAcceptances(rows, token)
        case .interactions, .schedules: return .none
        }
    }

    /// Every page of one section at one token. A token that moves between pages restarts the
    /// section so the rows never mix two revisions; a row the server cut short is read whole.
    private static func all<Row: Decodable & Sendable & Equatable & Identified>(
        _ section: DomainV2.Section, _ row: Row.Type, _ epoch: DomainV2.DataEpoch,
        _ transport: any DomainSyncTransport,
        complete: (@Sendable (String) async throws -> Row)? = nil
    ) async throws -> ([Row], String) {
        for _ in 0..<sectionAttempts {
            var rows: [Row] = []
            var token: String?
            var cursor: String?
            var truncated: Set<String> = []
            var pages = 0
            repeat {
                let page = try await transport.domainPage(section, of: row, cursor: cursor, pendingOnly: false)
                guard page.dataEpoch == epoch else {
                    throw DomainV2.DomainError(code: "stale_epoch", message: "Section read crossed an epoch", currentRevision: page.revision, subject: section.rawValue)
                }
                if token == nil { token = page.sectionRevision }
                guard token == page.sectionRevision else { token = nil; break }
                rows += page.items
                truncated.formUnion(page.truncated)
                pages += 1
                guard page.nextCursor != cursor || page.nextCursor == nil, pages < pageLimit else {
                    throw DomainV2.DomainError(code: "incompatible_protocol", message: "Section paging does not advance", currentRevision: page.revision, subject: section.rawValue)
                }
                cursor = page.nextCursor
            } while cursor != nil
            guard let token else { continue }
            if let complete, !truncated.isEmpty {
                for index in rows.indices where truncated.contains(rows[index].rowID) {
                    rows[index] = try await complete(rows[index].rowID)
                }
            }
            return (rows, token)
        }
        throw SectionReadError.unsettled(section.rawValue)
    }

    private mutating func readInteractions(epoch: DomainV2.DataEpoch, all: Bool,
                                            from transport: any DomainSyncTransport) async throws {
        var wanted: [DomainV2.InteractionId: DomainV2.Revision?] = [:]
        func visit(_ item: DomainV2.InboxItem) {
            if case .interaction(let id) = item.subject { wanted[id] = .some(item.revision) }
            item.children.forEach(visit)
        }
        if all {
            inbox.forEach(visit)
            for task in tasks where !task.archived {
                if let id = task.finalPresentation, wanted[id] == nil { wanted[id] = .some(nil) }
            }
        } else if let first = inbox.filter(\.pending).min(by: { ($0.order, $0.id) < ($1.order, $1.id) }) {
            visit(first)
        }
        var next: [DomainV2.InteractionId: DomainV2.Interaction] = [:]
        var missing: [DomainV2.InteractionId] = []
        for (id, hint) in wanted {
            if let cached = interactions[id], hint == nil || cached.revision == hint {
                next[id] = cached
            } else {
                missing.append(id)
            }
        }
        let fetched = try await withThrowingTaskGroup(of: DomainV2.Interaction?.self) { group in
            for id in missing {
                group.addTask {
                    do {
                        let item = try await transport.domainInteraction(id)
                        guard item.dataEpoch == epoch else {
                            throw DomainV2.DomainError(code: "stale_epoch", message: "Interaction read crossed an epoch", currentRevision: item.revision, subject: id.rawValue)
                        }
                        return item.item
                    } catch let error as DomainV2.DomainError where error.code == "not_found" {
                        // It left the projection since the section was read; the next summary drops it.
                        return nil
                    }
                }
            }
            var fetched: [DomainV2.Interaction] = []
            for try await interaction in group { if let interaction { fetched.append(interaction) } }
            return fetched
        }
        for interaction in fetched { next[interaction.id] = interaction }
        interactions = next
    }

    private func projection(_ summary: DomainV2.Summary) -> DomainV2.DomainProjection {
        DomainV2.DomainProjection(
            protocolVersion: summary.protocolVersion, dataEpoch: summary.dataEpoch, revision: summary.revision,
            inbox: inbox, counts: summary.counts, tasks: tasks, agents: agents, requests: requests,
            interactions: interactions.values.sorted { ($0.order, $0.id.rawValue) < ($1.order, $1.id.rawValue) },
            pullRequests: pullRequests, documents: documents.map(\.document),
            documentRevisions: documents.flatMap(\.revisions), documentReviews: documents.flatMap(\.reviews),
            mergeAuthorizations: mergeAuthorizations, stackReadiness: stackReadiness,
            supersededAcceptances: supersededAcceptances)
    }
}

private enum SectionRead: Sendable {
    case inbox([DomainV2.InboxItem], String)
    case tasks([DomainV2.Task], String)
    case requests([DomainV2.Request], String)
    case agents([DomainV2.Agent], String)
    case pullRequests([DomainV2.PullRequest], String)
    case documents([DomainV2.DocumentBundle], String)
    case mergeAuthorizations([DomainV2.MergeAuthorization], String)
    case stackReadiness([DomainV2.StackReadiness], String)
    case supersededAcceptances([DomainV2.SupersededAcceptance], String)
    case none
}

enum SectionReadError: Error, Equatable {
    /// The section kept changing while it was paged; the session retries after a backoff.
    case unsettled(String)
}

/// The key `Page.truncated` names a row by.
protocol Identified { var rowID: String { get } }
extension DomainV2.InboxItem: Identified { var rowID: String { id } }
extension DomainV2.Task: Identified { var rowID: String { id.rawValue } }
extension DomainV2.Request: Identified { var rowID: String { id.rawValue } }
extension DomainV2.Agent: Identified { var rowID: String { id.rawValue } }
extension DomainV2.PullRequest: Identified { var rowID: String { id.rawValue } }
extension DomainV2.DocumentBundle: Identified { var rowID: String { document.id.rawValue } }
extension DomainV2.MergeAuthorization: Identified { var rowID: String { id.rawValue } }
extension DomainV2.StackReadiness: Identified { var rowID: String { taskId.rawValue } }
extension DomainV2.SupersededAcceptance: Identified { var rowID: String { taskId.rawValue } }
