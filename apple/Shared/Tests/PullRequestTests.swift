import AlondraShared
import Foundation
import Testing

@Suite struct PullRequestTests {
    private func pr(_ extra: String) throws -> PullRequest {
        let json = """
        {"id":4,"url":"https://github.com/a/b/pull/12","repo":"a/b","number":12,"session_id":"s","title":"Menus","state":"open","opened_at":1,"base":"fase-1","head":"menus","additions":120,"deletions":8,"changed_files":5\(extra)}
        """
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(PullRequest.self, from: Data(json.utf8))
    }

    @Test func theMethodLastUsedInTheRepoComesFirst() throws {
        #expect(try pr(#","merge_methods":["merge","squash"]"#).methods == ["merge", "squash"])
        #expect(try pr(#","merge_methods":["merge","squash"],"preferred_method":"squash""#).methods == ["squash", "merge"])
        #expect(try pr(#","merge_methods":["merge","squash"],"preferred_method":"rebase""#).methods == ["merge", "squash"])
        #expect(try pr("").methods == PullRequest.allMethods, "all three when the repo was not read")
    }

    private func train(_ id: UInt64, openedAt: Int64, bottom: UInt64?, position: Int?, trainOpenedAt: Int64?) throws -> PullRequest {
        let below = position.map { $0 > 0 ? "\(id + 9)" : "null" } ?? "null"
        let json = """
        {"id":\(id),"url":"https://github.com/a/b/pull/\(id + 10)","repo":"a/b","number":\(id + 10),"session_id":"s","title":"T\(id)","state":"open","opened_at":\(openedAt),"below_number":\(below),"stack_bottom":\(bottom.map(String.init) ?? "null"),"stack_position":\(position.map(String.init) ?? "null"),"stack_opened_at":\(trainOpenedAt.map(String.init) ?? "null")}
        """
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(PullRequest.self, from: Data(json.utf8))
    }

    @Test func aPROnTopOfAnOpenPRSaysSoAndWaitsForIt() throws {
        let upper = try train(2, openedAt: 5, bottom: 1, position: 1, trainOpenedAt: 3)
        #expect(upper.stackedOn == "On top of PR #11")
        #expect(upper.mergeBlockedReason == "Merge PR #11 first")
        let setAsideBelow = try pr(#","below_number":11,"below_set_aside":true"#)
        #expect(setAsideBelow.mergeBlockedReason == "PR #11 is still open on GitHub: merge or close it first")
        let alone = try pr("")
        #expect(alone.stackedOn == nil)
        #expect(alone.mergeBlockedReason == nil)
    }

    @Test(arguments: [
        ("newest train first, each bottom first", [(9, 50, nil, nil, nil), (3, 60, 1, 2, 10), (1, 10, 1, 0, 10), (2, 70, 1, 1, 10), (4, 20, nil, nil, nil)], ["p9", "p4", "p1", "p2", "p3"], ["p1", "p2", "p3", "p4", "p9"]),
        ("no trains keeps opening order", [(1, 10, nil, nil, nil), (2, 20, nil, nil, nil)], ["p2", "p1"], ["p1", "p2"]),
        ("two trains opened together stay apart", [(4, 11, 3, 1, 10), (2, 12, 1, 1, 10), (1, 10, 1, 0, 10), (3, 10, 3, 0, 10)], ["p3", "p4", "p1", "p2"], ["p1", "p2", "p3", "p4"]),
    ] as [(String, [(UInt64, Int64, UInt64?, Int?, Int64?)], [String], [String])])
    func trainsAreListedTogetherBottomFirst(what: String, cards: [(UInt64, Int64, UInt64?, Int?, Int64?)], sidebar: [String], ring: [String]) throws {
        let prs = try cards.map { try train($0.0, openedAt: $0.1, bottom: $0.2, position: $0.3, trainOpenedAt: $0.4) }
        #expect(prs.sorted(by: PullRequest.newestTrainsFirst).map { InboxItem.pullRequest($0).id } == sidebar, "\(what): sidebar")
        #expect(InboxRing.ordered(prs.map(InboxItem.pullRequest)).map(\.id) == ring, "\(what): triage ring")
    }

    @Test func aCardSaysWhereItGoesAndHowBigItIs() throws {
        let p = try pr(#","checks":[{"name":"test","state":"failed","url":null}],"mergeable":"CONFLICTING""#)
        #expect(p.summary == "a/b #12 · fase-1 ← menus · +120 −8 in 5 files")
        #expect(p.hasConflicts)
        #expect(p.checks?.first?.failed == true)
        #expect(InboxItem.pullRequest(p).id == "p4")
    }

    @Test func aDirtyMergeStateIsAConflictAndUnknownIsStillBeingChecked() throws {
        #expect(try pr(#","mergeable":"UNKNOWN","merge_state":"DIRTY""#).hasConflicts)
        let checking = try pr(#","mergeable":"UNKNOWN","merge_state":"UNKNOWN""#)
        #expect(!checking.hasConflicts && checking.mergeabilityUnknown)
        let clean = try pr(#","mergeable":"MERGEABLE","merge_state":"CLEAN""#)
        #expect(!clean.hasConflicts && !clean.mergeabilityUnknown)
    }
}
