import AlondraShared
import Foundation
import Testing

@Suite struct PullRequestTests {
    private func pr(_ extra: String) throws -> PullRequest {
        let json = """
        {"id":4,"url":"https://github.com/a/b/pull/12","repo":"a/b","number":12,"session_id":"s","title":"Menus","state":"open","opened_at":1,"base":"main","head":"menus","additions":120,"deletions":8,"changed_files":5\(extra)}
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
        let alone = try pr("")
        #expect(alone.stackedOn == nil)
        #expect(alone.mergeBlockedReason == nil)
    }

    @Test func aCardSaysWhereItGoesAndHowBigItIs() throws {
        let p = try pr(#","checks":[{"name":"test","state":"failed","url":null}],"mergeable":"CONFLICTING""#)
        #expect(p.summary == "a/b #12 · main ← menus · +120 −8 in 5 files")
        #expect(p.hasConflicts)
        #expect(p.checks?.first?.failed == true)
    }

    /// 24 Sep: David's «la pantalla de PR full screen no tiene los mismos botones»; the card and
    /// the full screen now both ask the PR which main button to show.
    @Test(arguments: [
        ("ready", #","mergeable":"MERGEABLE","merge_methods":["merge","squash"],"preferred_method":"squash""#, PullRequest.MainAction.merge(methods: ["squash", "merge"], blockedBy: nil)),
        ("GitHub still checking", #","mergeable":"UNKNOWN""#, .merge(methods: PullRequest.allMethods, blockedBy: nil)),
        ("on top of an open PR", #","below_number":11"#, .merge(methods: PullRequest.allMethods, blockedBy: "Merge PR #11 first")),
        ("conflicts", #","mergeable":"CONFLICTING""#, .resolveConflicts),
        ("conflicts before the PR under it", #","merge_state":"DIRTY","below_number":11"#, .resolveConflicts),
    ])
    func conflictsComeFirstAndAMergeWaitsForThePRUnderIt(what: String, extra: String, expected: PullRequest.MainAction) throws {
        #expect(try pr(extra).mainAction == expected, "\(what)")
    }

    @Test func aDirtyMergeStateIsAConflictAndUnknownIsStillBeingChecked() throws {
        #expect(try pr(#","mergeable":"UNKNOWN","merge_state":"DIRTY""#).hasConflicts)
        let checking = try pr(#","mergeable":"UNKNOWN","merge_state":"UNKNOWN""#)
        #expect(!checking.hasConflicts && checking.mergeabilityUnknown)
        let clean = try pr(#","mergeable":"MERGEABLE","merge_state":"CLEAN""#)
        #expect(!clean.hasConflicts && !clean.mergeabilityUnknown)
    }
}
