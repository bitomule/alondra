import AlondraContract
import AlondraFeatures
import AlondraFeed
import AlondraiOSUI
import ComposableArchitecture
import Foundation
import SwiftUI

/// Waiting for You with two «Do» cards, each answered with Done, for UI tests that touch it the way
/// a person does. Every answer the screen sends is listed, by card and option, in `answers`; the
/// card in front, whether the stack is still moving and what it received are in `stack`.
@main
struct TriageTouchHostApp: App {
    static let frontID = "t-holepunch-ports-1"
    static let backID = "t-holepunch-xcode-iphone-1"

    @State private var answers = Answers()

    init() {
        UserDefaults.standard.set(["en-US"], forKey: "AppleLanguages")
        UserDefaults.standard.set("en_US", forKey: "AppleLocale")
    }

    var body: some Scene {
        WindowGroup {
            Host(answers: answers)
        }
    }
}

@MainActor
@Observable
final class Answers {
    var sent: [String] = []
    let trail: Trail
    let store: StoreOf<TriageFeature>

    init() {
        let documents = Self.documents()
        let recorder = Recorder()
        let trail = Trail()
        self.trail = trail
        store = Store(initialState: TriageFeature.State()) {
            Reduce { _, action in
                MainActor.assumeIsolated { trail.record(action) }
                return .none
            }
            TriageFeature()
        } withDependencies: {
            $0.viewsClient = ViewsClient(
                load: { key, _ in
                    guard let document = documents[key.rawValue] else { throw ViewsClientError.missingView }
                    return .document(document)
                },
                open: { _ in }, openScreen: { _, _ in }, closeScreen: { _ in },
                feed: { AsyncStream { $0.finish() } },
                observe: { key in
                    AsyncThrowingStream { continuation in
                        guard let document = documents[key.rawValue] else {
                            return continuation.finish(throwing: ViewsClientError.missingView)
                        }
                        continuation.yield(.document(document))
                    }
                },
                reconnections: { AsyncStream { _ in } },
                connection: { AsyncStream { $0.yield(.connected(.thisMac)) } },
                problem: { _ in .unexpected },
                prepare: { $0 },
                perform: { request in
                    await recorder.record(request)
                    return try ContractCodec.decode(ActionResponse.self, from: Data(
                        #"{"protocol":1,"outcome":"applied","message":"Done","stamp":"Done"}"#.utf8))
                },
                retryNow: {},
                stallAfter: { nil },
                undeliveredCards: { AsyncStream { $0.finish() } },
                setAsideAnswers: { nil },
                dismissSetAsideAnswers: {},
                fetchContent: { _ in "" },
                fetchDiff: { _ in FetchedDiff(text: "") },
                setFileViewed: { _, _, _ in .unviewed },
                fetchAttachment: { _ in Data() }
            )
        }
        recorder.answers = self
    }

    private static func documents() -> [String: ViewDocument] {
        let ids = [TriageTouchHostApp.frontID, TriageTouchHostApp.backID]
        var documents = ["home": home(queue: ids)]
        for (index, id) in ids.enumerated() {
            documents["card:\(id)"] = doCard(id: id, need: index == 0 ? "Open ports 443 and 7842 on the router" : "Sign in to Xcode on the Mac")
        }
        return documents
    }

    private static func json(_ name: String) -> [String: Any] {
        let url = Bundle.main.url(forResource: name, withExtension: "json")!
        return try! JSONSerialization.jsonObject(with: Data(contentsOf: url)) as! [String: Any]
    }

    private static func decode(_ object: [String: Any]) -> ViewDocument {
        try! ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }

    private static func home(queue: [String]) -> ViewDocument {
        var object = json("home-ios")
        var sections = object["sections"] as! [[String: Any]]
        sections[0]["card_ids"] = queue
        sections[0]["card_waits"] = queue.map { _ in "question" }
        object["sections"] = sections
        return decode(object)
    }

    /// A «Do» card as the daemon draws a request for an action: Done first, then I can't do it.
    private static func doCard(id: String, need: String) -> ViewDocument {
        var object = json("card-question")
        object["key"] = "card:\(id)"
        object["title"] = need
        var sections = object["sections"] as! [[String: Any]]
        var items = sections[0]["items"] as! [[String: Any]]
        var card = items[0]
        card["id"] = id
        card["kind"] = "do"
        card["need"] = need
        card["why"] = "The iPhone reaches the Mac from outside home only through these ports."
        card.removeValue(forKey: "content_row")
        card["reply"] = (card["reply"] as? [String: Any]).map { reply in
            var reply = reply
            reply["id"] = "reply:\(id)@1"
            return reply
        }
        card["more"] = []
        card["actions"] = [("done", "Done", "primary", "advance"), ("cannot", "I can't do it", "secondary", "neutral")].map { option, label, style, role in
            [
                "id": "ans:\(id)@1", "label": label, "style": style, "role": role, "option_id": option,
                "presentation": "button", "input": ["kind": "none"], "attachments": false, "enabled": true,
                "stamp": "Done", "resolves_card": true,
            ] as [String: Any]
        }
        items[0] = card
        sections[0]["items"] = items
        sections = [sections[0]]
        object["sections"] = sections
        return decode(object)
    }
}

/// What the stack received and did, in order, so a test can wait for a swipe to settle and a
/// failure says whether the touch arrived.
@MainActor
@Observable
final class Trail {
    var events: [String] = []

    func record(_ action: TriageFeature.Action) {
        switch action {
        case .task: events.append("task")
        case .disappeared: events.append("disappeared")
        case .leaveForLater: events.append("later")
        case .dragChanged: events.append("dragChanged")
        case .dragEnded(let translation, let predicted): events.append("dragEnded(\(Int(translation)),\(Int(predicted)))")
        case .swipeSettled: events.append("settled")
        default: break
        }
    }
}

/// Hands each request the screen delivers to the label the UI test reads.
final class Recorder: @unchecked Sendable {
    @MainActor weak var answers: Answers?

    func record(_ request: ActionRequest) async {
        let card = request.actionId.replacingOccurrences(of: "ans:", with: "").components(separatedBy: "@")[0]
        await MainActor.run { answers?.sent.append("\(card):\(request.optionId ?? "")") }
    }
}

private struct Host: View {
    let answers: Answers
    @Namespace private var zoom

    var body: some View {
        NavigationStack {
            TriageView(store: answers.store, zoom: zoom)
        }
        .overlay(alignment: .bottom) {
            Text(answers.sent.isEmpty ? "none" : answers.sent.joined(separator: ","))
                .font(.caption2)
                .accessibilityIdentifier("answers")
                .allowsHitTesting(false)
        }
        .overlay(alignment: .top) {
            Text(stack)
                .font(.caption2)
                .lineLimit(1)
                .accessibilityIdentifier("stack")
                .allowsHitTesting(false)
        }
    }

    private var stack: String {
        let store = answers.store
        return "front=\(store.currentID ?? "none");settled=\(store.swipe == nil);trail=\(answers.trail.events.joined(separator: "|"))"
    }
}
