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
