import AlondraViews
import AlondraViewsDemoWorkspace
import Testing

@Suite struct ViewsDemoWorkspaceTests {
    @Test func homeCardsAndLinkedScreensUseViewsDocuments() async throws {
        let client = ViewsDemoWorkspace.client()
        guard case let .document(home) = try await client.load(.home, nil) else {
            Issue.record("The demo home must provide a Views document")
            return
        }
        let ids = try #require(home.sections.first { $0.id == "queue" }?.cardIds)
        #expect(ids == ["i-1", "i-2", "i-3", "pr-1"])
        #expect(home.sections.first { $0.id == "queue" }?.items.isEmpty == true)

        for id in ids {
            guard case let .document(card) = try await client.load(.card(id), nil) else {
                Issue.record("Missing demo card \(id)")
                continue
            }
            #expect(card.key == ViewKey.card(id).rawValue)
            #expect(!card.sections.isEmpty)
        }

        for key in [ViewKey.tasks, .fleet, .node("a-1"), .task("t-1"), .request("r-1")] {
            guard case let .document(screen) = try await client.load(key, nil) else {
                Issue.record("Missing demo destination \(key.rawValue)")
                continue
            }
            #expect(screen.key == key.rawValue)
            #expect(!screen.sections.isEmpty)
        }

        for (key, expectedLink) in [(ViewKey.tasks, "task:t-1"), (.fleet, "node:a-1")] {
            guard case let .document(screen) = try await client.load(key, nil) else {
                Issue.record("Missing demo list \(key.rawValue)")
                continue
            }
            let links = screen.sections.flatMap(\.items).compactMap { item -> Link? in
                if case let .row(row) = item { return row.link }
                return nil
            }
            #expect(links.contains(.view(key: expectedLink)))
        }
    }

    @Test func pullRequestOpensDiffAndContextNoteIsOptional() async throws {
        let client = ViewsDemoWorkspace.client()
        guard case let .document(pr) = try await client.load(.pullRequest("pr-1"), nil) else {
            Issue.record("Missing demo pull request")
            return
        }
        let items = pr.sections.flatMap(\.items)
        let diff = try #require(items.compactMap { item -> ContentRef? in
            if case let .contentRef(value) = item, value.kind == .diff { return value }
            return nil
        }.first)
        #expect(diff.fetch == "/pull-requests/pr-1/diff")
        #expect(try await client.fetchContent(diff).contains("diff --git"))
        let actions = items.flatMap { item -> [AlondraViews.Action] in
            if case let .actions(bar) = item { return bar.actions }
            return []
        }
        #expect(actions.contains { $0.label == "Request changes" && $0.comments?.kind == .diffLines })
        #expect(actions.allSatisfy { $0.link == nil })

        guard case let .document(prCardView) = try await client.load(.card("pr-1"), nil),
              case let .card(prCard) = try #require(prCardView.sections.flatMap(\.items).first) else {
            Issue.record("Missing demo PR home card")
            return
        }
        let review = try #require(prCard.actions.first { $0.label == "Request changes" })
        #expect(review.comments?.kind == .diffLines)
        #expect(review.enabled)
        #expect(review.link == .view(key: "pull_request:pr-1"))

        guard case let .document(cardView) = try await client.load(.card("i-1"), nil),
              case let .card(card) = try #require(cardView.sections.flatMap(\.items).first) else {
            Issue.record("Missing demo question card")
            return
        }
        let context = try #require(card.actions.first { $0.label == "Need more context" })
        guard case let .text(_, required, _) = context.input else {
            Issue.record("The context action must retain its text input")
            return
        }
        #expect(!required)
    }
}
