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

    /// Brings every synced section to the summary's tokens and returns the assembled view.
    mutating func refresh(to summary: DomainV2.Summary, from transport: any DomainSyncTransport) async throws -> DomainV2.DomainProjection {
        if epoch != summary.dataEpoch { self = DomainSections() }
        guard summary.revision >= revision else {
            throw DomainV2.DomainError(code: "incompatible_protocol", message: "Domain revision moved backwards within one epoch",
                                       currentRevision: summary.revision, subject: nil)
        }
        epoch = summary.dataEpoch
        for section in Self.synced {
            let token = summary.sections[section.rawValue]?.revision
            guard token == nil || tokens[section] != token else { continue }
            try await read(section, epoch: summary.dataEpoch, from: transport)
        }
        try await readInteractions(epoch: summary.dataEpoch, from: transport)
        revision = summary.revision
        return projection(summary)
    }

    private mutating func read(_ section: DomainV2.Section, epoch: DomainV2.DataEpoch,
                               from transport: any DomainSyncTransport) async throws {
        switch section {
        case .inbox: (inbox, tokens[section]) = try await Self.all(section, DomainV2.InboxItem.self, epoch, transport)
        case .tasks: (tasks, tokens[section]) = try await Self.all(section, DomainV2.Task.self, epoch, transport)
        case .requests:
            (requests, tokens[section]) = try await Self.all(section, DomainV2.Request.self, epoch, transport) { id in
                try await transport.domainRequest(.init(rawValue: id)).item
            }
        case .agents: (agents, tokens[section]) = try await Self.all(section, DomainV2.Agent.self, epoch, transport)
        case .pullRequests: (pullRequests, tokens[section]) = try await Self.all(section, DomainV2.PullRequest.self, epoch, transport)
        case .documents: (documents, tokens[section]) = try await Self.all(section, DomainV2.DocumentBundle.self, epoch, transport)
        case .mergeAuthorizations: (mergeAuthorizations, tokens[section]) = try await Self.all(section, DomainV2.MergeAuthorization.self, epoch, transport)
        case .stackReadiness: (stackReadiness, tokens[section]) = try await Self.all(section, DomainV2.StackReadiness.self, epoch, transport)
        case .supersededAcceptances: (supersededAcceptances, tokens[section]) = try await Self.all(section, DomainV2.SupersededAcceptance.self, epoch, transport)
        case .interactions, .schedules: return
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

    private mutating func readInteractions(epoch: DomainV2.DataEpoch, from transport: any DomainSyncTransport) async throws {
        var wanted: [DomainV2.InteractionId: DomainV2.Revision?] = [:]
        func visit(_ item: DomainV2.InboxItem) {
            if case .interaction(let id) = item.subject { wanted[id] = .some(item.revision) }
            item.children.forEach(visit)
        }
        inbox.forEach(visit)
        for task in tasks where !task.archived {
            if let id = task.finalPresentation, wanted[id] == nil { wanted[id] = .some(nil) }
        }
        var next: [DomainV2.InteractionId: DomainV2.Interaction] = [:]
        for (id, hint) in wanted.sorted(by: { $0.key.rawValue < $1.key.rawValue }) {
            if let cached = interactions[id], hint == nil || cached.revision == hint {
                next[id] = cached
                continue
            }
            do {
                let item = try await transport.domainInteraction(id)
                guard item.dataEpoch == epoch else {
                    throw DomainV2.DomainError(code: "stale_epoch", message: "Interaction read crossed an epoch", currentRevision: item.revision, subject: id.rawValue)
                }
                next[id] = item.item
            } catch let error as DomainV2.DomainError where error.code == "not_found" {
                // It left the projection since the section was read; the next summary drops it.
            }
        }
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
