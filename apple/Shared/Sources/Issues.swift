import AlondraContracts
import Foundation
import Observation

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
