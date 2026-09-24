import AlondraShared
import Foundation
import Testing

@Suite struct BackgroundTests {
    private func node(_ background: String?) throws -> Node {
        let extra = background.map { #","background":\#($0)"# } ?? ""
        let json = #"{"session_id":"s","short_id":"s","alive":true,"is_orchestrator":false,"has_plugin":true,"state":"working","state_since":1,"state_source":"hooks"\#(extra)}"#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(Node.self, from: Data(json.utf8))
    }

    @Test func aNodeFromAnOlderServerRunsNothing() throws {
        let n = try node(nil)
        #expect(n.running.isEmpty)
        #expect(n.backgroundBadge == nil)
    }

    @Test func theBadgeCountsEachKindInAFixedOrder() throws {
        let n = try node(#"[{"kind":"shell","label":"bazelisk build","started_at":10},{"kind":"agent","label":"Review","started_at":20},{"kind":"monitor","label":"tail","started_at":30},{"kind":"agent","label":"Audit","started_at":40}]"#)
        let badge = try #require(n.backgroundBadge)
        #expect(badge.counts.map(\.kind) == [.agent, .shell, .other])
        #expect(badge.counts.map(\.count) == [2, 1, 1])
        #expect(!badge.suspect)
        #expect(badge.text.hasPrefix("2 subagents · "))
        #expect(!badge.text.contains(BackgroundItem.possiblyHung))
    }

    @Test func aPossiblyHungItemMarksTheBadge() throws {
        let n = try node(#"[{"kind":"shell","label":"until pgrep","started_at":10,"suspect":true},{"kind":"shell","label":"sleep","started_at":20}]"#)
        #expect(n.running.map(\.isSuspect) == [true, false])
        let badge = try #require(n.backgroundBadge)
        #expect(badge.suspect)
        #expect(badge.text == "2 shells · \(BackgroundItem.possiblyHung)")
    }
}
