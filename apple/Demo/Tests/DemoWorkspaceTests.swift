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
        #expect(ids.count == 5)
        #expect(state.cards[id: ids[0]]?.card?.kind == .decide)
        #expect(state.cards[id: ids[2]]?.card?.kind == .review)
        #expect(state.cards[id: ids[0]]?.card?.need != state.cards[id: ids[2]]?.card?.need)
        store.send(.disappeared)
    }

    @Test(arguments: [false, true])
    func theMacSidebarLoadsFromTheFilesTheAppBundles(spanish: Bool) async throws {
        let client = ViewsClient.demo(spanish: spanish)
        guard case let .document(sidebar) = try await client.load(.sidebar, nil) else {
            Issue.record("The demo sidebar must load")
            return
        }
        #expect(sidebar.client == .mac)
        let cardKeys = sidebar.sections.flatMap(\.items).compactMap { item -> ViewKey? in
            guard case let .row(row) = item, case let .view(key)? = row.link, key.hasPrefix("card:") else { return nil }
            return ViewKey(rawValue: key)
        }
        #expect(!cardKeys.isEmpty)
        for key in cardKeys {
            guard case .document = try await client.load(key, nil) else {
                Issue.record("The sidebar card \(key.rawValue) must open in the demo")
                continue
            }
        }
    }

    @Test func aSpanishDeviceReadsTheNewRequestScreenInSpanish() async throws {
        guard case let .document(spanish) = try await ViewsClient.demo(spanish: true).load(.newRequest(), nil),
              case let .document(english) = try await ViewsClient.demo().load(.newRequest(), nil) else {
            Issue.record("The demo New screen must load")
            return
        }
        let queue = { (document: ViewDocument) in document.sections.first { $0.id == "sent" } }
        #expect(queue(spanish)?.title == "Solicitudes pendientes")
        #expect(queue(english)?.title == "Pending Requests")
        let badges = queue(spanish)?.items.compactMap { item -> [String]? in
            if case let .row(row) = item { row.badges.map(\.text) } else { nil }
        }.flatMap { $0 }
        #expect(badges == ["En cola"])
    }

    @Test func homeCardsAndLinkedScreensUseViewsDocuments() async throws {
        let client = ViewsClient.demo()
        guard case let .document(home) = try await client.load(.home, nil) else {
            Issue.record("The demo home must provide a Views document")
            return
        }
        let ids = try #require(home.sections.first { $0.id == "queue" }?.cardIds)
        #expect(ids.count == 5)
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
        #expect(questionCard.need == "Choose the welcome copy")
        #expect(questionCard.actions.compactMap(\.optionId) == ["clear", "warm"])
        guard case let .document(information) = try await client.load(.card(ids[3]), nil) else {
            Issue.record("The information demo card must load")
            return
        }
        let informationCard = try #require(information.sections.flatMap(\.items).compactMap { item -> Card? in
            if case let .card(card) = item { card } else { nil }
        }.first)
        #expect(informationCard.kind == .know)
        func linkedID(_ link: Link?) -> String {
            guard case let .view(key)? = link else {
                Issue.record("The demo context must link to its screen")
                return ""
            }
            return String(key.split(separator: ":").last ?? "")
        }
        let taskFact = question.sections.flatMap(\.items).compactMap { item -> Fact? in
            if case let .facts(facts) = item { facts.items.first { $0.label == "Task" } } else { nil }
        }.first
        let taskID = linkedID(taskFact?.link)
        #expect(linkedID(questionCard.context.first { $0.icon == .task }?.link) == taskID)
        for key in [ViewKey.tasks, .task(taskID),
                    .newRequest(), .machine, .schedules, .schedule("new")] {
            guard case let .document(screen) = try await client.load(key, nil) else {
                Issue.record("Missing demo destination \(key.rawValue)")
                continue
            }
            #expect(screen.key == key.rawValue)
            #expect(!screen.sections.isEmpty)
        }

        for (key, expectedLink) in [(ViewKey.tasks, "task:\(taskID)")] {
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
        #expect(deliveryCard.need == "Review the beta candidate")
    }

    @Test func pullRequestOpensDiffAndItsCardRequestsChangesInItsField() async throws {
        let client = ViewsClient.demo()
        let prID = "github.com/example/repo#12"
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
        #expect(try await client.fetchDiff(diff).text.contains("diff --git"))
        let actions = items.flatMap { item -> [AlondraContract.Action] in
            if case let .actions(bar) = item { return bar.actions }
            return []
        }
        #expect(actions.contains { $0.label == "Request changes" && $0.comments?.kind == .diffLines })

        guard case let .document(prCardView) = try await client.load(.card(prID), nil),
              case let .card(prCard) = try #require(prCardView.sections.flatMap(\.items).first) else {
            Issue.record("Missing demo PR home card")
            return
        }
        let review = try #require(prCard.reply)
        #expect(review.label == "Request changes")
        #expect(review.comments?.kind == .diffLines)
        #expect(review.enabled)
        #expect(review.link == .view(key: "pull_request:\(prID)"))
    }
}
