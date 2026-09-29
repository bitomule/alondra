import AlondraContracts
import AlondraDemoWorkspace
import AlondraDomain
import AlondraShared
import ComposableArchitecture
import Foundation
import Testing

@Suite struct DemoWorkspaceTests {
    private let question = DomainV2.InteractionId(rawValue: "demo-question")

    private func pendingQuestions(_ workspace: DomainWorkspace?) -> [String] {
        (workspace?.roots ?? []).filter { $0.kind == "question" && $0.pending }.map(\.id)
    }

    @Test func answeringThroughFeatureDependencyPublishesOnlyInThatSyntheticSession() async throws {
        let first = try DemoWorkspace()
        let isolated = try DemoWorkspace()
        var dependencies = DependencyValues()
        first.install(into: &dependencies)
        var iterator = await dependencies.domainWorkspace.observe().makeAsyncIterator()
        let initial = try #require(await iterator.next()?.workspace)
        #expect(pendingQuestions(initial) == ["interaction:demo-question"])

        let outcome = await dependencies.domainWorkspace.perform(.answer(question, optionID: "calmer", inputs: [:]), initial, [], "command-1")
        guard case .recorded = outcome else { Issue.record("The demo records a valid answer: \(outcome)"); return }
        let changed = try #require(await iterator.next()?.workspace)
        #expect(pendingQuestions(changed).isEmpty)
        #expect(changed.projection.interactions.contains { $0.body.contains("Your demo answer: calmer") })
        #expect(pendingQuestions(await isolated.workspace()) == ["interaction:demo-question"])
        await first.close(); await isolated.close()
    }

    @Test func aRetryWithTheSameCommandIDReturnsTheFirstResultAndAppliesOnce() async throws {
        let demo = try DemoWorkspace()
        var dependencies = DependencyValues()
        demo.install(into: &dependencies)
        let workspace = try #require(await demo.workspace())
        let update = DomainV2.InteractionId(rawValue: "demo-update")

        let first = await dependencies.domainWorkspace.perform(.markRead(update), workspace, [], "read-1")
        let retry = await dependencies.domainWorkspace.perform(.markRead(update), workspace, [], "read-1")
        let another = await dependencies.domainWorkspace.perform(.markRead(update), workspace, [], "read-2")

        guard case .recorded = first else { Issue.record("First read must be recorded: \(first)"); return }
        #expect(retry == first, "The same command ID returns the first outcome instead of reading twice")
        guard case .rejected = another else { Issue.record("A new command ID is a second read: \(another)"); return }

        let before = try #require(await demo.workspace()).projection.interactions.count
        let message = DomainIntent.messageAgent("demo-design", body: "Hello", attachments: [])
        _ = await dependencies.domainWorkspace.perform(message, workspace, [], "message-1")
        _ = await dependencies.domainWorkspace.perform(message, workspace, [], "message-1")
        let after = try #require(await demo.workspace()).projection.interactions.count
        #expect(after == before + 1)
        await demo.close()
    }

    @Test func resetDropsMutationAuthorityAndAReplacementStartsClean() async throws {
        let old = try DemoWorkspace()
        var dependencies = DependencyValues()
        old.install(into: &dependencies)
        let workspace = try #require(await old.workspace())
        let attachmentID = UUID()
        _ = try await old.storeAttachment(id: attachmentID, name: "example.txt", data: Data("sample".utf8))
        await old.close()
        #expect(await old.workspace() == nil)
        #expect(await dependencies.domainWorkspace.perform(.answer(question, optionID: "keep", inputs: [:]), workspace, [], "stale") == .obsolete)
        await #expect(throws: CancellationError.self) {
            _ = try await old.storeAttachment(id: attachmentID, name: "example.txt", data: Data("late".utf8))
        }
        let replacement = try DemoWorkspace()
        #expect(replacement.identity.credentialRevision != old.identity.credentialRevision)
        #expect(pendingQuestions(await replacement.workspace()) == ["interaction:demo-question"])
        await replacement.close()
    }
}
