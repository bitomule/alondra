import AlondraShared
import Foundation
import Testing

@Suite struct ArtifactTests {
    static let cases: [(String, String, [String])] = [
        ("a bare link", "https://claude.ai/artifact/abc123", ["https://claude.ai/artifact/abc123"]),
        ("a Claude Code artifact", "https://claude.ai/code/artifact/0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b", ["https://claude.ai/code/artifact/0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b"]),
        ("inside Markdown", "Plan: [the page](https://claude.ai/artifact/XyZ_9-a).", ["https://claude.ai/artifact/XyZ_9-a"]),
        ("ending a sentence", "Read it at https://claude.ai/artifact/abc123.", ["https://claude.ai/artifact/abc123"]),
        ("with a query", "https://claude.ai/artifact/abc123?tab=2", ["https://claude.ai/artifact/abc123"]),
        ("without a scheme", "see claude.ai/artifact/abc123 please", ["https://claude.ai/artifact/abc123"]),
        ("two, repeated", "https://claude.ai/artifact/b and https://claude.ai/code/artifact/a, again https://claude.ai/artifact/b", ["https://claude.ai/artifact/b", "https://claude.ai/code/artifact/a"]),
        ("no id", "https://claude.ai/artifact/ is empty", []),
        ("another host", "https://notclaude.ai/artifact/abc and https://claude.ai/chat/abc", []),
        ("a PR", "https://github.com/bitomule/alondra/pull/61", []),
    ]

    @Test(arguments: cases)
    func artifactLinksAreFoundWhereverANodeWritesThem(_ c: (String, String, [String])) {
        #expect(Artifacts.urls(in: c.1).map(\.absoluteString) == c.2, "\(c.0)")
    }

    @Test func aReportCarriesTheArtifactsInItsLinksAndItsBodyOnce() throws {
        let json = """
        {"id":1,"session_id":"s","title":"Plan","kind":"document","created_at":0,"attachments":[],
         "body":"Also https://claude.ai/code/artifact/two and https://claude.ai/artifact/one",
         "links":[{"url":"https://claude.ai/artifact/one","title":"The plan"},{"url":"https://github.com/a/b/pull/1"}]}
        """
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let report = try decoder.decode(Report.self, from: Data(json.utf8))
        #expect(report.artifacts.map(\.absoluteString) == ["https://claude.ai/artifact/one", "https://claude.ai/code/artifact/two"])
    }
}
