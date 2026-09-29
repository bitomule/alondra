import AlondraContracts
import Foundation

/// The resource read grammar mirrored from the daemon's `domain_reads::parse`.
public enum DomainReadPath {
    public static func isValid(_ path: String) -> Bool {
        guard path.utf8.count <= 4096 else { return false }
        let parts = path.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false)
        let query = parts.count == 2 ? String(parts[1]) : nil
        guard parts[0].hasPrefix("/"), !parts[0].hasPrefix("//") else { return false }
        let encoded = parts[0].dropFirst().split(separator: "/", omittingEmptySubsequences: false).map(String.init)
        // Segments are percent-encoded on the wire; the daemon checks the decoded id.
        let segments = encoded.compactMap { $0.removingPercentEncoding }
        guard segments.count == encoded.count, (1...3).contains(segments.count) else { return false }
        let page = ["cursor", "limit"]
        switch (segments[0], segments.count > 1 ? segments[1] : nil, segments.count > 2 ? segments[2] : nil) {
        case ("summary", nil, nil): return valid(query, allowed: ["include"])
        case ("inbox", nil, nil): return valid(query, allowed: page + ["filter"])
        case ("inbox", let id?, nil): return query == nil && isID(id, extra: [58], length: 512)
        case ("tasks", nil, nil): return valid(query, allowed: page + ["request", "agent"])
        case ("tasks", let id?, nil): return query == nil && isID(id)
        case ("tasks", let id?, "interactions"): return isID(id) && valid(query, allowed: page)
        case ("interactions", nil, nil): return query?.split(separator: "&").contains("scope=orchestrator") == true
            && valid(query, allowed: ["scope"] + page)
        case ("interactions", let id?, nil), ("requests", let id?, nil), ("pull-requests", let id?, nil),
             ("documents", let id?, nil):
            return query == nil && isID(id)
        case ("document-revisions", let id?, "content"):
            return isID(id) && valid(query, allowed: ["offset", "version"])
        case ("interactions", let id?, "body"), ("requests", let id?, "body"):
            return isID(id) && query?.hasPrefix("offset=") == true && valid(query, allowed: ["offset"])
        case ("requests", nil, nil), ("agents", nil, nil), ("pull-requests", nil, nil), ("documents", nil, nil),
             ("merge-authorizations", nil, nil), ("stack-readiness", nil, nil), ("superseded-acceptances", nil, nil):
            return valid(query, allowed: page)
        default: return false
        }
    }

    private static func isID(_ id: String, extra: Set<UInt8> = [], length: Int = 256) -> Bool {
        !id.isEmpty && id.utf8.count <= length && id.utf8.allSatisfy { byte in
            (48...57).contains(byte) || (65...90).contains(byte) || (97...122).contains(byte)
                || byte == 45 || byte == 95 || extra.contains(byte)
        }
    }

    private static func valid(_ query: String?, allowed: [String]) -> Bool {
        guard let query else { return true }
        var seen: Set<String> = []
        for field in query.split(separator: "&", omittingEmptySubsequences: false) {
            let pair = field.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false).map(String.init)
            guard pair.count == 2, !pair[1].isEmpty, allowed.contains(pair[0]), seen.insert(pair[0]).inserted else { return false }
            let value = pair[1]
            switch pair[0] {
            case "cursor": guard value.utf8.count <= 512, isID(value, length: 512) else { return false }
            case "limit": guard value.utf8.count <= 3, let limit = Int(value), (1...200).contains(limit) else { return false }
            case "filter": guard value == "pending" else { return false }
            case "request", "agent": guard isID(value) else { return false }
            case "scope": guard value == "orchestrator" else { return false }
            case "offset": guard value.utf8.count <= 19, value.utf8.allSatisfy({ (48...57).contains($0) }), UInt64(value) != nil else { return false }
            case "version": guard isID(value, length: 128) else { return false }
            case "include":
                let names = value.split(separator: ",", omittingEmptySubsequences: false).map(String.init)
                guard Set(names).count == names.count,
                      names.allSatisfy({ $0 != "schedules" && DomainV2.Section(rawValue: $0) != nil }) else { return false }
            default: return false
            }
        }
        return true
    }
}

extension DomainV2.Section {
    var route: String {
        switch self {
        case .pullRequests: "pull-requests"
        case .mergeAuthorizations: "merge-authorizations"
        case .stackReadiness: "stack-readiness"
        case .supersededAcceptances: "superseded-acceptances"
        default: rawValue
        }
    }
}

extension AlondraAPIClient {
    public func domainSummary() async throws -> DomainV2.Summary {
        let summary = try await send(Endpoint<DomainV2.Summary>(.get, path: ["summary"]) {
            try DomainV2.decode(DomainV2.Summary.self, from: $0)
        })
        try Self.check(summary.protocolVersion, summary.revision)
        return summary
    }

    /// One page of a section. `filter` is only meaningful for the inbox (`pending`).
    public func domainPage<Row: Decodable & Sendable & Equatable>(
        _ section: DomainV2.Section, of row: Row.Type, cursor: String? = nil, pendingOnly: Bool = false
    ) async throws -> DomainV2.Page<Row> {
        guard section != .interactions, section != .schedules else {
            throw DomainV2.DomainError(code: "invalid_input", message: "Section is not paged here", currentRevision: nil, subject: section.rawValue)
        }
        var query: [(String, String)] = []
        if let cursor { query.append(("cursor", cursor)) }
        if pendingOnly, section == .inbox { query.append(("filter", "pending")) }
        let page = try await send(Endpoint<DomainV2.Page<Row>>(.get, path: [section.route], query: query) {
            try DomainV2.decode(DomainV2.Page<Row>.self, from: $0)
        })
        try Self.check(page.protocolVersion, page.revision)
        guard page.section == section.rawValue else {
            throw DomainV2.DomainError(code: "incompatible_protocol", message: "Unexpected section", currentRevision: page.revision, subject: page.section)
        }
        return page
    }

    public func domainTaskInteractions(_ task: DomainV2.TaskId, cursor: String? = nil) async throws -> DomainV2.Page<DomainV2.Interaction> {
        let page = try await send(Endpoint<DomainV2.Page<DomainV2.Interaction>>(.get,
            path: ["tasks", task.rawValue, "interactions"], query: cursor.map { [("cursor", $0)] } ?? []) {
                try DomainV2.decode(DomainV2.Page<DomainV2.Interaction>.self, from: $0)
            })
        try Self.check(page.protocolVersion, page.revision)
        return page
    }

    public func domainTaskDetail(_ task: DomainV2.TaskId) async throws -> DomainV2.TaskDetail {
        let detail = try await send(Endpoint<DomainV2.TaskDetail>(.get, path: ["tasks", task.rawValue]) {
            try DomainV2.decode(DomainV2.TaskDetail.self, from: $0)
        })
        try Self.check(detail.protocolVersion, detail.revision)
        return detail
    }

    public func domainPullRequestDetail(_ pr: DomainV2.PrId) async throws -> DomainV2.PullRequestDetail {
        let detail = try await send(Endpoint<DomainV2.PullRequestDetail>(.get, path: ["pull-requests", pr.rawValue]) {
            try DomainV2.decode(DomainV2.PullRequestDetail.self, from: $0)
        })
        try Self.check(detail.protocolVersion, detail.revision)
        return detail
    }

    /// One inbox entry, root or child, with its current actions and the root that holds it.
    public func domainInboxItem(_ id: String) async throws -> DomainV2.InboxLookup {
        let lookup = try await send(Endpoint<DomainV2.InboxLookup>(.get, path: ["inbox", id]) {
            try DomainV2.decode(DomainV2.InboxLookup.self, from: $0)
        })
        try Self.check(lookup.protocolVersion, lookup.revision)
        return lookup
    }

    /// The whole interaction: a row cut to fit the budget is completed from `…/body` chunks.
    public func domainInteraction(_ id: DomainV2.InteractionId) async throws -> DomainV2.Item<DomainV2.Interaction> {
        var item = try await send(Endpoint<DomainV2.Item<DomainV2.Interaction>>(.get, path: ["interactions", id.rawValue]) {
            try DomainV2.decode(DomainV2.Item<DomainV2.Interaction>.self, from: $0)
        })
        try Self.check(item.protocolVersion, item.revision)
        if item.truncated {
            let body = try await domainBody(["interactions", id.rawValue, "body"], id: id.rawValue)
            item.item.body = body
        }
        return item
    }

    public func domainRequest(_ id: DomainV2.RequestId) async throws -> DomainV2.Item<DomainV2.Request> {
        var item = try await send(Endpoint<DomainV2.Item<DomainV2.Request>>(.get, path: ["requests", id.rawValue]) {
            try DomainV2.decode(DomainV2.Item<DomainV2.Request>.self, from: $0)
        })
        try Self.check(item.protocolVersion, item.revision)
        if item.truncated {
            let body = try await domainBody(["requests", id.rawValue, "body"], id: id.rawValue)
            item.item.body = body
        }
        return item
    }

    /// Chunks carry byte offsets into one body; every chunk must agree on the size and the
    /// last one closes it exactly.
    func domainBody(_ path: [String], id: String) async throws -> String {
        var collected = Data()
        var total: UInt64?
        repeat {
            let chunk = try await send(Endpoint<DomainV2.TextChunk>(.get, path: path, query: [("offset", String(collected.count))]) {
                try DomainV2.decode(DomainV2.TextChunk.self, from: $0)
            })
            let bytes = Data(chunk.data.utf8)
            guard chunk.protocolVersion == DomainV2.protocolVersion, chunk.id == id,
                  chunk.offset == UInt64(collected.count), total == nil || total == chunk.totalBytes,
                  chunk.offset + UInt64(bytes.count) <= chunk.totalBytes,
                  !bytes.isEmpty || !chunk.hasMore
            else {
                throw DomainV2.DomainError(code: "content_unavailable", message: "Text changed while it was read", currentRevision: chunk.revision, subject: id)
            }
            total = chunk.totalBytes
            collected.append(bytes)
            if !chunk.hasMore {
                guard UInt64(collected.count) == chunk.totalBytes, let text = String(data: collected, encoding: .utf8) else {
                    throw DomainV2.DomainError(code: "content_unavailable", message: "Text changed while it was read", currentRevision: chunk.revision, subject: id)
                }
                return text
            }
        } while true
    }

    private static func check(_ version: UInt32, _ revision: DomainV2.Revision) throws {
        guard version == DomainV2.protocolVersion, revision <= DomainV2.maximumRevision else {
            throw DomainV2.DomainError(code: "incompatible_protocol", message: "Unsupported domain read", currentRevision: nil, subject: nil)
        }
    }
}
