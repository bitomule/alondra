import AlondraContracts
import Foundation

extension IssueSummary {
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
