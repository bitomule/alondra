import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct ViewedFilesFeatureTests {
    private nonisolated static let head = String(repeating: "a", count: 40)
    private nonisolated static let marks = FileMarks(prID: "github.com/example/repo#12", head: head,
                                         files: ["Sources/App.swift": .unviewed, "Sources/Old.swift": .viewed, "Sources/New.swift": .changed])
    private nonisolated static let diffText = ["Sources/App.swift", "Sources/Old.swift", "Sources/New.swift"].map { path in
        "diff --git a/\(path) b/\(path)\n--- a/\(path)\n+++ b/\(path)\n@@ -1 +1 @@\n-old\n+new\n"
    }.joined()

    private func review(marks: FileMarks? = Self.marks, saving: @escaping @Sendable (Bool, String, FileMarks) async throws -> FileViewed,
                        diffs: LockIsolated<Int> = LockIsolated(0), clock: TestClock<Duration> = TestClock()) throws -> TestStoreOf<PRReviewFeature> {
        let store = TestStore(initialState: PRReviewFeature.State(key: .pullRequest("github.com/example/repo#12"))) {
            PRReviewFeature()
        } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.fetchDiff = { _ in
                diffs.withValue { $0 += 1 }
                return FetchedDiff(text: Self.diffText, marks: marks)
            }
            $0.viewsClient.setFileViewed = saving
        }
        store.exhaustivity = .off
        return store
    }

    @Test func viewedFilesStartFoldedAndChangedOnesOpen() async throws {
        let store = try review(saving: { _, _, _ in .viewed })

        await store.send(.screen(.loaded(try Fixtures.document("pull-request-ios"))))
        await store.receive(\.diffLoaded)

        #expect(store.state.collapsed == ["Sources/Old.swift"])
        #expect(store.state.viewed.marks == Self.marks.files)
    }

    @Test func aTickShowsAtOnceFoldsTheFileAndIsSavedInGitHub() async throws {
        let saves = LockIsolated<[String]>([])
        let gate = AsyncStream<Void>.makeStream()
        let store = try review(saving: { viewed, path, marks in
            saves.withValue { $0.append("\(viewed) \(path) \(marks.head)") }
            for await _ in gate.stream { break }
            return .viewed
        })
        await store.send(.screen(.loaded(try Fixtures.document("pull-request-ios"))))
        await store.receive(\.diffLoaded)

        await store.send(.viewed(.toggle("Sources/App.swift")))
        #expect(store.state.viewed.marks?["Sources/App.swift"] == .viewed, "the box is ticked before GitHub answers")
        #expect(store.state.collapsed.contains("Sources/App.swift"))

        gate.continuation.yield()
        await store.receive(\.viewed.saved)
        #expect(store.state.viewed.pending.isEmpty)
        #expect(store.state.viewed.saved?.files["Sources/App.swift"] == .viewed)
        #expect(saves.value == ["true Sources/App.swift \(Self.head)"])
    }

    @Test func gitHubFailingIsRetriedQuietlyAndOnlyAfterThirtySecondsTheBoxGoesBack() async throws {
        struct Down: Error {}
        let attempts = LockIsolated(0)
        let clock = TestClock()
        let store = try review(saving: { _, _, _ in
            attempts.withValue { $0 += 1 }
            throw Down()
        }, clock: clock)
        await store.send(.screen(.loaded(try Fixtures.document("pull-request-ios"))))
        await store.receive(\.diffLoaded)

        await store.send(.viewed(.toggle("Sources/App.swift")))
        await clock.advance(by: .seconds(29))
        #expect(attempts.value > 1)
        #expect(store.state.viewed.marks?["Sources/App.swift"] == .viewed)
        #expect(store.state.viewed.failed == nil)

        await clock.advance(by: .seconds(1))
        await store.receive(\.viewed.gaveUp)
        #expect(store.state.viewed.marks?["Sources/App.swift"] == .unviewed)
        #expect(store.state.viewed.failed == "Sources/App.swift")
    }

    @Test func anOldHeadFetchesTheDiffAgain() async throws {
        let diffs = LockIsolated(0)
        let store = try review(saving: { _, _, _ in throw FileMarkError.stale }, diffs: diffs)
        await store.send(.screen(.loaded(try Fixtures.document("pull-request-ios"))))
        await store.receive(\.diffLoaded)

        await store.send(.viewed(.toggle("Sources/App.swift")))
        await store.receive(\.viewed.stale)
        await store.receive(\.diffLoaded)

        #expect(diffs.value == 2)
        #expect(store.state.viewed.marks?["Sources/App.swift"] == .unviewed)
    }

    @Test func withoutBoxesFromTheMacNothingCanBeTicked() async throws {
        let saves = LockIsolated(0)
        let store = try review(marks: nil, saving: { _, _, _ in
            saves.withValue { $0 += 1 }
            return .viewed
        })
        await store.send(.screen(.loaded(try Fixtures.document("pull-request-ios"))))
        await store.receive(\.diffLoaded)

        await store.send(.viewed(.toggle("Sources/App.swift")))

        #expect(store.state.viewed.marks == nil)
        #expect(store.state.collapsed.isEmpty)
        #expect(saves.value == 0)
    }
}
