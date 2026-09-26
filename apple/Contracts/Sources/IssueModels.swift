import Foundation

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

}

public struct IssueList: Decodable, Sendable {
    public let fetchedAt: Int64?
    public let issues: [IssueSummary]
    public let error: String?
}
