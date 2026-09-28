import AlondraContracts
import AlondraDomain
import AlondraPullRequestsFeature
import ComposableArchitecture
import Foundation
import Testing

private func workspace() throws -> DomainWorkspace {
    let env = ProcessInfo.processInfo.environment
    let url: URL
    if let runfiles = env["TEST_SRCDIR"] {
        url = URL(fileURLWithPath: runfiles).appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("apple/APIClient/Tests/domain-v2-wire-fixtures.json")
    } else {
        url = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("APIClient/Tests/domain-v2-wire-fixtures.json")
    }
    let all = try JSONDecoder().decode([String: DomainV2.DomainProjection].self, from: Data(contentsOf: url))
    return DomainWorkspace(try #require(all["pr_question"]))
}

private func card(_ workspace: DomainWorkspace) throws -> DomainPullRequestCard {
    let root = try #require(workspace.item("pr:pr"))
    return try #require(DomainPullRequestCard(root: root, in: workspace))
}

private actor Recorder {
    var intents: [DomainIntent] = []
    var diffs: [String] = []
    func record(_ intent: DomainIntent) { intents.append(intent) }
    func diff(_ head: String) { diffs.append(head) }
}

private let diffText = "diff --git a/a.swift b/a.swift\n--- a/a.swift\n+++ b/a.swift\n@@ -1,1 +1,1 @@\n-old\n+new\n"

private func client(_ recorder: Recorder, result: DomainCommandResult, head: String = "head1") -> DomainWorkspaceClient {
    var client = DomainWorkspaceClient.inert
    client.perform = { intent, _, _ in
        await recorder.record(intent)
        return result
    }
    client.pullRequestDiff = { id, expected in
        await recorder.diff(expected)
        return DomainV2.PullRequestDiff(prId: id, host: "github.com", repository: "owner/repo", number: 1,
                                        head: head, diff: diffText, truncated: false)
    }
    return client
}

private actor Counter {
    var value = 0
    func next() -> Int { value += 1; return value }
}

private let recorded = DomainCommandResult.recorded(.init(revision: 8, changed: true, effects: [], createdIds: []))

@MainActor
struct PullRequestFeatureTests {
    @Test
    func cardKeepsTheChildQuestionSeparateFromThePresentation() throws {
        let card = try card(workspace())
        #expect(card.presentationID == "d")
        #expect(card.questions.map(\.id) == ["q"])
        #expect(card.presentedHead == "head1")
        #expect(card.canAuthorizeMerge)
        #expect(card.pullRequest.allowedMethods == [.squash])
    }

    @Test
    func authorizingAMergeRecordsAnAuthorizationNotAMerge() async throws {
        let recorder = Recorder()
        let workspace = try workspace()
        let initial = PullRequestFeature.State(card: try card(workspace), workspace: workspace)
        let store = TestStore(initialState: initial) { PullRequestFeature() } withDependencies: {
            $0.domainWorkspace = client(recorder, result: recorded)
        }
        store.exhaustivity = .off
        await store.send(.mergeMethodConfirmed(.merge))
        #expect(await recorder.intents.isEmpty, "A method the PR does not allow is never sent")
        await store.send(.mergeMethodConfirmed(.squash))
        await store.receive(\.mutationFinished)
        #expect(await recorder.intents == [.authorizeMerge("d", method: .squash)])
        #expect(store.state.mutationOutcome == .confirmed)
        #expect(store.state.pr?.state == .open, "The PR stays open until the server observes the merge")
    }

    @Test
    func uncertainAuthorizationBlocksASecondOne() async throws {
        let recorder = Recorder()
        let command = UUID()
        let workspace = try workspace()
        let initial = PullRequestFeature.State(card: try card(workspace), workspace: workspace)
        let store = TestStore(initialState: initial) { PullRequestFeature() } withDependencies: {
            $0.domainWorkspace = client(recorder, result: .uncertain(command))
        }
        store.exhaustivity = .off
        await store.send(.mergeMethodConfirmed(.squash))
        await store.receive(\.mutationFinished)
        #expect(!store.state.canAct)
        await store.send(.mergeMethodConfirmed(.squash))
        #expect(await recorder.intents.count == 1)
    }

    @Test
    func requestingChangesSendsFeedbackOnThePresentedHeadAfterRecheckingTheDiff() async throws {
        let recorder = Recorder()
        let workspace = try workspace()
        let initial = DiffReviewFeature.State(card: try card(workspace), workspace: workspace)
        let store = TestStore(initialState: initial) { DiffReviewFeature() } withDependencies: {
            $0.domainWorkspace = client(recorder, result: recorded)
        }
        store.exhaustivity = .off
        await store.send(.appeared)
        await store.receive(\.diffLoaded)
        await store.send(.noteChanged("Rename new"))
        await store.send(.requestChangesTapped)
        await store.receive(\.reviewFinished)
        #expect(await recorder.diffs == ["head1", "head1"])
        #expect(await recorder.intents == [.submitFeedback("d", body: "Rename new")])
    }

    @Test
    func aChangedDiffStopsTheFeedbackAndKeepsTheDraft() async throws {
        let recorder = Recorder()
        let workspace = try workspace()
        let initial = DiffReviewFeature.State(card: try card(workspace), workspace: workspace)
        let counter = Counter()
        let store = TestStore(initialState: initial) { DiffReviewFeature() } withDependencies: {
            var value = client(recorder, result: recorded)
            value.pullRequestDiff = { id, _ in
                let calls = await counter.next()
                return DomainV2.PullRequestDiff(prId: id, host: "github.com", repository: "owner/repo", number: 1,
                                                head: "head1", diff: calls == 1 ? diffText : diffText + "+more\n", truncated: false)
            }
            $0.domainWorkspace = value
        }
        store.exhaustivity = .off
        await store.send(.appeared)
        await store.receive(\.diffLoaded)
        await store.send(.noteChanged("Keep me"))
        await store.send(.requestChangesTapped)
        await store.receive(\.reviewDiffChanged)
        #expect(await recorder.intents.isEmpty)
        #expect(store.state.note == "Keep me")
    }
}
