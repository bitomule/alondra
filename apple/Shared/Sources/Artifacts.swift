import Foundation

public enum Artifacts {
    private static let kinds = ["claude.ai/code/artifact/", "claude.ai/artifact/"]

    public static var openInBrowserLabel: String {
        String(localized: "Open in Browser", table: "AlondraShared", bundle: #bundle)
    }

    public static func urls(in text: String) -> [URL] {
        var found: [(offset: Int, url: String)] = []
        for kind in kinds {
            var searchStart = text.startIndex
            while let range = text.range(of: kind, range: searchStart..<text.endIndex) {
                searchStart = range.upperBound
                let before = text[text.startIndex..<range.lowerBound]
                let startsHost = before.hasSuffix("https://") || before.hasSuffix("http://") || before.hasSuffix("www.")
                    || !(before.last.map { $0.isLetter || $0.isNumber || $0 == "." || $0 == "-" } ?? false)
                guard startsHost else { continue }
                let id = text[range.upperBound...].prefix { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_") }
                guard !id.isEmpty else { continue }
                let url = "https://\(kind)\(id)"
                if !found.contains(where: { $0.url == url }) {
                    found.append((text.distance(from: text.startIndex, to: range.lowerBound), url))
                }
            }
        }
        return found.sorted { $0.offset < $1.offset }.compactMap { URL(string: $0.url) }
    }
}
