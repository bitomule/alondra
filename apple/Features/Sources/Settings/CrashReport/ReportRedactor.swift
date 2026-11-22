import Foundation

/// Redacts values by `ReportShapes`: the same private value always gets the same pseudonym.
/// `prefix` keeps an app's pseudonyms apart from the daemon's in the same report.
public struct ReportRedactor {
    public let vocabulary: Set<String>
    private let prefix: String
    private var names: [String: String] = [:]
    private var counts: [ReportShapes.Kind: Int] = [:]

    public init(vocabulary: Set<String>, prefix: String = "") {
        self.vocabulary = vocabulary; self.prefix = prefix
    }

    public mutating func pseudonym(_ kind: ReportShapes.Kind, _ value: String) -> String {
        if let name = names[value] { return name }
        let count = counts[kind, default: 0] + 1
        counts[kind] = count
        let name = "\(prefix)\(kind.rawValue)-\(count)"
        names[value] = name
        return name
    }

    /// Itself when every token is safe, the safe tokens with each id, path or URL replaced when that
    /// is all it has, or one pseudonym for the whole of it.
    public mutating func value(_ value: String, hint: ReportShapes.Kind, known: Set<String> = []) -> String {
        if ReportShapes.isISOTime(value) { return ReportShapes.utc(value) ?? pseudonym(.value, value) }
        let parts = ReportShapes.joinCardGroups(ReportShapes.split(value))
        let tokens = parts.compactMap { if case .token(let token) = $0 { token } else { nil } }
        let vocabulary = vocabulary.union(known)
        guard tokens.allSatisfy({ ReportShapes.isSafe($0, vocabulary: vocabulary) || ReportShapes.kindOfShape($0) != nil
            || ReportShapes.isPrivateNumber($0) }) else {
            return pseudonym(ReportShapes.kindOfShape(value) ?? hint, value)
        }
        var out = ""
        for part in parts {
            switch part {
            case .gap(let gap): out += gap
            case .token(let token) where ReportShapes.isISOTime(token): out += ReportShapes.utc(token) ?? pseudonym(.value, token)
            case .token(let token) where ReportShapes.isSafe(token, vocabulary: vocabulary): out += token
            case .token(let token): out += shaped(token)
            }
        }
        return out
    }

    /// A field that holds a version (the app's own): its version-shaped tokens stay as they are, even
    /// a triple that free text would take for the start of an address.
    public mutating func versionField(_ value: String) -> String {
        self.value(value, hint: .value, known: Set(ReportShapes.tokens(value).filter(ReportShapes.isVersion)))
    }

    private mutating func shaped(_ token: String) -> String {
        if let pull = ReportShapes.pullRequest(token) { return "\(pseudonym(.repo, pull.repo))/pull/\(pull.number)" }
        return pseudonym(ReportShapes.kindOfShape(token) ?? .value, token)
    }
}
