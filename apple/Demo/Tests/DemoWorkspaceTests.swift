import AlondraContract
import AlondraDemo
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Testing

@Suite struct DemoWorkspaceTests {
    @MainActor
    @Test func triageReducerKeepsDistinctNeighbouringCards() async throws {
        let client = ViewsClient.demo()
        let store = withDependencies {
            $0.viewsClient = client
        } operation: {
            Store(initialState: TriageFeature.State()) { TriageFeature() }
        }
        await store.send(.task).finish()
        let state = store.withState { $0 }
        let ids = state.orderedIDs
        #expect(ids.count == 4)
        #expect(state.cards[id: ids[0]]?.card?.icon == .question)
        #expect(state.cards[id: ids[1]]?.card?.icon == .notice)
        #expect(state.cards[id: ids[0]]?.card?.body?.text != state.cards[id: ids[1]]?.card?.body?.text)
        store.send(.disappeared)
    }

    @Test func homeCardsAndLinkedScreensUseViewsDocuments() async throws {
        let client = ViewsClient.demo()
        guard case let .document(home) = try await client.load(.home, nil) else {
            Issue.record("The demo home must provide a Views document")
            return
        }
        let ids = try #require(home.sections.first { $0.id == "queue" }?.cardIds)
        #expect(ids.count == 4)
        #expect(home.sections.first { $0.id == "queue" }?.items.isEmpty == true)

        for id in ids {
            guard case let .document(card) = try await client.load(.card(id), nil) else {
                Issue.record("Missing demo card \(id)")
                continue
            }
            #expect(card.key == ViewKey.card(id).rawValue)
            #expect(!card.sections.isEmpty)
        }

        guard case let .document(question) = try await client.load(.card(ids[0]), nil) else {
            Issue.record("The first demo card must load")
            return
        }
        let questionCard = try #require(question.sections.flatMap(\.items).compactMap { item -> Card? in
            if case let .card(card) = item { card } else { nil }
        }.first)
        #expect(questionCard.title == "Restore the native review experience")
        #expect(questionCard.body?.text.contains("Body 0") == false)
        let answer = try #require(questionCard.actions.first { $0.presentation == .options })
        #expect(answer.options.map(\.label) == ["Ship the concise copy", "Keep the warmer copy"])
        guard case let .document(information) = try await client.load(.card(ids[1]), nil) else {
            Issue.record("The information demo card must load")
            return
        }
        let informationCard = try #require(information.sections.flatMap(\.items).compactMap { item -> Card? in
            if case let .card(card) = item { card } else { nil }
        }.first)
        #expect(informationCard.icon == .notice)
        #expect(informationCard.body?.text.contains("simulator pass completed") == true)
        func contextID(_ icon: Icon) throws -> String {
            let context = try #require(questionCard.context.first { $0.icon == icon })
            guard case let .view(key)? = context.link else {
                Issue.record("The demo context must link to its screen")
                return ""
            }
            return String(key.split(separator: ":").last ?? "")
        }
        let nodeID = try contextID(.node)
        let taskID = try contextID(.task)
        for key in [ViewKey.tasks, .fleet, .node(nodeID), .task(taskID),
                    .newRequest(), .machine, .schedules, .schedule("new")] {
            guard case let .document(screen) = try await client.load(key, nil) else {
                Issue.record("Missing demo destination \(key.rawValue)")
                continue
            }
            #expect(screen.key == key.rawValue)
            #expect(!screen.sections.isEmpty)
        }

        for (key, expectedLink) in [(ViewKey.tasks, "task:\(taskID)"), (.fleet, "node:\(nodeID)")] {
            guard case let .document(screen) = try await client.load(key, nil) else {
                Issue.record("Missing demo list \(key.rawValue)")
                continue
            }
            func links(in row: Row) -> [Link] {
                (row.link.map { [$0] } ?? []) + row.children.flatMap(links)
            }
            let links = screen.sections.flatMap(\.items).flatMap { item -> [Link] in
                if case let .row(row) = item { return links(in: row) }
                return []
            }
            #expect(links.contains(.view(key: expectedLink)))
        }

        guard case let .document(newRequest) = try await client.load(.newRequest(), nil) else {
            Issue.record("Missing production-shaped new request fixture")
            return
        }
        let form = try #require(newRequest.sections.flatMap(\.items).compactMap { item -> Form? in
            if case let .form(form) = item { form } else { nil }
        }.first)
        #expect(form.fields.contains { $0.id == "pr" })
        #expect(form.fields.contains { $0.id == "effort" })

        let deliveryID = try #require(ids.dropFirst(2).first)
        guard case let .document(delivery) = try await client.load(.card(deliveryID), nil),
              case let .card(deliveryCard) = try #require(delivery.sections.flatMap(\.items).first) else {
            Issue.record("The demo delivery card must load")
            return
        }
        #expect(deliveryCard.title == "Restore the native review experience")
        #expect(deliveryCard.body?.text.contains("The candidate is ready for your final approval.") == true)
    }

    @Test func pullRequestOpensDiffAndContextNoteIsOptional() async throws {
        let client = ViewsClient.demo()
        guard case let .document(home) = try await client.load(.home, nil) else {
            Issue.record("The demo home must load")
            return
        }
        let prID = try #require(home.sections.first?.cardIds?.last)
        guard case let .document(pr) = try await client.load(.pullRequest(prID), nil) else {
            Issue.record("Missing demo pull request")
            return
        }
        let items = pr.sections.flatMap(\.items)
        let diff = try #require(items.compactMap { item -> ContentRef? in
            if case let .contentRef(value) = item, value.kind == .diff { return value }
            return nil
        }.first)
        #expect(diff.fetch == "/pull-requests/github.com%2Fexample%2Frepo%2312/diff")
        #expect(try await client.fetchContent(diff).contains("diff --git"))
        let actions = items.flatMap { item -> [AlondraContract.Action] in
            if case let .actions(bar) = item { return bar.actions }
            return []
        }
        #expect(actions.contains { $0.label == "Request changes" && $0.comments?.kind == .diffLines })
        #expect(actions.contains { $0.id?.hasPrefix("ask_pr:\(prID)@") == true })

        guard case let .document(prCardView) = try await client.load(.card(prID), nil),
              case let .card(prCard) = try #require(prCardView.sections.flatMap(\.items).first) else {
            Issue.record("Missing demo PR home card")
            return
        }
        let review = try #require(prCard.actions.first { $0.label == "Request changes" })
        #expect(review.comments?.kind == .diffLines)
        #expect(review.enabled)
        #expect(review.link == .view(key: "pull_request:\(prID)"))

        let ids = try #require(home.sections.first?.cardIds)
        guard case let .document(cardView) = try await client.load(.card(ids[0]), nil),
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
