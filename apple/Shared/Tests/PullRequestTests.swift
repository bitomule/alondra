import AlondraContracts
import AlondraDomain
import AlondraShared
import Foundation
import Testing

@Suite struct PullRequestTests {
    private static func pr(_ id: String, task: String, number: UInt64, state: DomainV2.PrState = .open,
                           methods: [DomainV2.MergeMethod] = [.squash, .merge]) -> DomainV2.PullRequest {
        .init(id: .init(rawValue: id), taskId: .init(rawValue: task), host: "github.com", repository: "a/b", number: number,
              head: "h-\(id)", base: "main", state: state, mergedHead: nil, allowedMethods: methods, review: nil)
    }

    private static func task(_ id: String, stackedOn below: String? = nil) -> DomainV2.Task {
        .init(id: .init(rawValue: id), requestId: "r", agentId: "a", title: "Task \(id)", state: .running, revision: 1,
              started: true, reviewPrs: false, finalPresentation: nil, prId: nil, dependencies: [],
              stack: below.map { .init(upstreamTask: .init(rawValue: "t-\($0)"), presentationId: "p", prId: .init(rawValue: $0), head: "h-\($0)") },
              documents: [:], archived: false)
    }

    private static func card(stackedOn below: DomainV2.PrState? = nil, runtime: DomainV2.PullRequestRuntime? = nil) throws -> DomainPullRequestCard {
        var pullRequests = [pr("top", task: "t-top", number: 12)]
        var tasks = [task("t-top", stackedOn: below.map { _ in "below" })]
        if let below {
            pullRequests.append(pr("below", task: "t-below", number: 11, state: below))
            tasks.append(task("t-below"))
        }
        let root = DomainV2.InboxItem(id: "pr:top", kind: "pull_request", subject: .pullRequest(prId: "top"), title: "Menus",
                                      body: nil, taskId: "t-top", requestId: "r", agentId: "a", revision: 1, order: 1,
                                      pending: true, children: [], availableActions: [])
        let projection = DomainV2.DomainProjection(
            protocolVersion: 2, dataEpoch: "e", revision: 1, inbox: [root], counts: .init(roots: 1, pending: 1, unreadInformation: 0),
            tasks: tasks, agents: [], requests: [], interactions: [], pullRequests: pullRequests, documents: [],
            documentRevisions: [], documentReviews: [], mergeAuthorizations: [], stackReadiness: [])
        let workspace = DomainWorkspace(projection, runtime: runtime.map { .init(pullRequests: ["top": $0]) })
        return try #require(DomainPullRequestCard(root: root, in: workspace))
    }

    @Test func aCardSaysWhereItGoes() throws {
        let card = try Self.card()
        #expect(card.summary == "a/b #12 · main")
        #expect(card.webURL?.absoluteString == "https://github.com/a/b/pull/12")
    }

    @Test func aPROnTopOfAnOpenPRSaysSoAndWaitsForIt() throws {
        let upper = try Self.card(stackedOn: .open)
        #expect(upper.stackedOn?.stackedOnLabel == "On top of PR #11")
        #expect(upper.mainAction == .merge(methods: [.squash, .merge], blockedBy: "Merge PR #11 first"))
        let landed = try Self.card(stackedOn: .merged)
        #expect(landed.stackedOn == nil, "a merged PR underneath no longer holds it")
        #expect(try Self.card().mainAction == .merge(methods: [.squash, .merge], blockedBy: nil))
    }

    @Test func conflictsComeFirstEvenOnTopOfAnotherPR() throws {
        let conflicted = try Self.card(stackedOn: .open, runtime: .init(conflicts: true))
        #expect(conflicted.mainAction == .resolveConflicts)
        let checking = try Self.card(runtime: .init(mergeabilityUnknown: true))
        #expect(checking.mainAction == .merge(methods: [.squash, .merge], blockedBy: nil), "GitHub still checking is not a conflict")
    }

    @Test(arguments: [
        ("conflicts", DomainV2.PullRequestRuntime(checks: [.init(name: "t", state: "failed")], conflicts: true), UInt64?.none, false, "PR #12 · conflicts"),
        ("failed check", .init(checks: [.init(name: "t", state: "failed")]), nil, false, "PR #12 · checks failed"),
        ("stacked", .init(), 11, false, "PR #12 · merge #11 first"),
        ("running", .init(checks: [.init(name: "a", state: "passed"), .init(name: "b", state: "running")]), nil, false, "PR #12 · waiting for checks, 1 of 2"),
        ("auto merge", .init(), nil, true, "PR #12 · Alondra merges it when the checks pass"),
        ("ready", .init(), nil, false, "PR #12 · ready to merge"),
    ])
    func progressSaysWhatHoldsThePR(what: String, runtime: DomainV2.PullRequestRuntime, below: UInt64?, mergesAlone: Bool, expected: String) {
        #expect(runtime.progress(number: 12, stackedOn: below, mergesAlone: mergesAlone) == expected, "\(what)")
    }
}
