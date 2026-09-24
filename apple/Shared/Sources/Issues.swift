import Foundation
import Observation

/// An open GitHub issue as `GET /v1/issues` lists it.
public struct IssueSummary: Decodable, Identifiable, Hashable, Sendable {
    public let repo: String
    public let number: Int
    public let title: String
    public let url: String
    public let author: String?
    public let labels: [String]
    public let comments: Int

    public var id: String { reference }
    public var reference: String { "\(repo)#\(number)" }

    enum CodingKeys: String, CodingKey {
        case repo, number, title, url, author, labels, comments
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        repo = try c.decode(String.self, forKey: .repo)
        number = try c.decode(Int.self, forKey: .number)
        title = try c.decode(String.self, forKey: .title)
        url = try c.decode(String.self, forKey: .url)
        author = try c.decodeIfPresent(String.self, forKey: .author)
        labels = try c.decodeIfPresent([String].self, forKey: .labels) ?? []
        comments = try c.decodeIfPresent(Int.self, forKey: .comments) ?? 0
    }

    /// Every word of the query appears in the repo, number, title or labels, as the TUI filters.
    public func matches(_ query: String) -> Bool {
        let hay = "\(reference) \(title) \(labels.joined(separator: " "))".lowercased()
        return query.lowercased().split(whereSeparator: \.isWhitespace).allSatisfy { hay.contains($0) }
    }

    /// `owner/repo#n` for an issue link or the short form itself; anything else is not an issue.
    public static func reference(in text: String) -> String? {
        let s = text.trimmingCharacters(in: .whitespacesAndNewlines).trimmingCharacters(in: CharacterSet(charactersIn: ".,)>"))
        if s.hasPrefix("https://github.com/") {
            let parts = s.dropFirst("https://github.com/".count).split(whereSeparator: { "/?#".contains($0) }).map(String.init)
            guard parts.count >= 4, parts[2] == "issues", let n = Int(parts[3]) else { return nil }
            return "\(parts[0])/\(parts[1])#\(n)"
        }
        let halves = s.split(separator: "#", omittingEmptySubsequences: false)
        guard halves.count == 2, let n = Int(halves[1]) else { return nil }
        let repo = halves[0].split(separator: "/", omittingEmptySubsequences: false)
        guard repo.count == 2, !repo[0].isEmpty, !repo[1].isEmpty else { return nil }
        return "\(halves[0])#\(n)"
    }

    /// An issue typed or pasted in the search box that the list does not already show.
    public static func pasted(in query: String, besides issues: [IssueSummary]) -> String? {
        reference(in: query).flatMap { ref in issues.contains { $0.reference == ref } ? nil : ref }
    }

    /// The project an issue belongs to: the checkout named like its repo, if there is one.
    public static func project(of reference: String?, among projects: [String]) -> String? {
        guard let name = reference?.split(separator: "#").first?.split(separator: "/").last else { return nil }
        return projects.first { $0.caseInsensitiveCompare(name) == .orderedSame }
    }
}

public struct IssueList: Decodable, Sendable {
    public let fetchedAt: Int64?
    public let issues: [IssueSummary]
    public let error: String?
}

/// The issue list of "New": fetched when the picker opens, again only from its refresh
/// button, and filtered here as the user types.
@MainActor
@Observable
public final class IssuesModel {
    public var query = ""
    public private(set) var issues: [IssueSummary] = []
    public private(set) var error: String?
    public private(set) var loading = false
    public private(set) var loaded = false
    private let client: AlondraClient

    public init(client: AlondraClient) {
        self.client = client
    }

    public var visible: [IssueSummary] {
        issues.filter { $0.matches(query) }
    }

    /// A link or `owner/repo#n` typed in the search box, when it names an issue not in the list.
    public var pasted: String? {
        IssueSummary.pasted(in: query, besides: issues)
    }

    public func load(refresh: Bool = false) async {
        guard !loading, refresh || !loaded else { return }
        loading = true
        defer { loading = false }
        do {
            let list = try await client.issues(refresh: refresh)
            issues = list.issues
            error = list.error
            loaded = true
        } catch {
            self.error = error.localizedDescription
        }
    }
}
