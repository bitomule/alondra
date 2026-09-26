import AlondraDemoWorkspace
import AlondraQuestionsFeature
import AlondraShared
import ComposableArchitecture
import Foundation
import Testing

@Suite struct DemoWorkspaceTests {
    @Test func answeringThroughFeatureDependencyPublishesOnlyInThatSyntheticSession() async throws {
        let first = try DemoWorkspace()
        let isolated = try DemoWorkspace()
        let updates = await first.snapshots()
        var iterator = updates.makeAsyncIterator()
        let initial = try #require(await iterator.next())
        #expect(initial?.openQuestions.map(\.id) == [1])
        var dependencies = DependencyValues()
        first.install(into: &dependencies)
        let outcome = await dependencies.questionWorkService.submit(.answer(id: 1, text: "Calmer layout", option: "Calmer layout", attachments: []))
        #expect(outcome == .confirmed)
        let changed = try #require(await iterator.next())
        #expect(changed?.openQuestions.isEmpty == true)
        #expect(changed?.news.contains(where: { $0.body?.contains("Calmer layout") == true }) == true)
        #expect(await isolated.snapshot()?.openQuestions.map(\.id) == [1])
        await first.close(); await isolated.close()
    }

    @Test func resetDropsMutationAuthorityAndAReplacementStartsClean() async throws {
        let old = try DemoWorkspace()
        var dependencies = DependencyValues()
        old.install(into: &dependencies)
        let attachmentID = UUID()
        _ = try await old.storeAttachment(id: attachmentID, name: "example.txt", data: Data("sample".utf8))
        await old.close()
        #expect(await old.snapshot() == nil)
        #expect(await dependencies.questionWorkService.submit(.answer(id: 1, text: "stale", option: nil, attachments: [])) == .obsolete)
        await #expect(throws: CancellationError.self) {
            _ = try await old.storeAttachment(id: attachmentID, name: "example.txt", data: Data("late".utf8))
        }
        let replacement = try DemoWorkspace()
        #expect(replacement.identity.credentialRevision != old.identity.credentialRevision)
        #expect(await replacement.snapshot()?.openQuestions.map(\.id) == [1])
        await replacement.close()
    }
}
