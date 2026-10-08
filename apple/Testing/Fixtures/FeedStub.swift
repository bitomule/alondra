import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation
import XCTest

/// What a screen under test sees of the daemon: the documents it is given, served by key, and
/// every action the screen sends recorded.
final class FeedStub: @unchecked Sendable {
    private let lock = NSLock()
    private var documents: [String: ViewDocument]
    private var sent: [ActionRequest] = []

    init(_ documents: [ViewDocument] = []) {
        self.documents = Dictionary(uniqueKeysWithValues: documents.map { ($0.key, $0) })
    }

    var requests: [ActionRequest] { lock.withLock { sent } }

    /// What a screen handed to the app as it left, which the app sends on its behalf.
    func deliver(_ request: ActionRequest) {
        lock.withLock { sent.append(request) }
    }

    func serve(_ document: ViewDocument) {
        lock.withLock { documents[document.key] = document }
    }

    var observe: @Sendable (ViewKey) -> AsyncThrowingStream<ViewFetch, any Error> {
        { [self] key in
            AsyncThrowingStream { continuation in
                if let document = lock.withLock({ documents[key.rawValue] }) {
                    continuation.yield(.document(document))
                    continuation.finish()
                } else {
                    continuation.finish(throwing: ViewsClientError.missingView)
                }
            }
        }
    }

    var load: @Sendable (ViewKey, String?) async throws -> ViewFetch {
        { [self] key, _ in
            guard let document = lock.withLock({ documents[key.rawValue] }) else { throw ViewsClientError.missingView }
            return .document(document)
        }
    }

    func perform(_ outcome: String = #"{"protocol":1,"outcome":"applied","message":"Done","stamp":"Answered"}"#)
        -> @Sendable (ActionRequest) async throws -> ActionResponse {
        { [self] request in
            lock.withLock { sent.append(request) }
            return try ContractCodec.decode(ActionResponse.self, from: Data(outcome.utf8))
        }
    }
}

extension Fixtures {
    static let questionID = "a98d034b-249e-4487-a89b-57de38c26372"
    static let informationID = "f12d034b-249e-4487-a89b-57de38c26372"
    static let deliveryID = "hf-weekly-2026-09-30"
    static let pullRequestID = "github.com/example/repo#12"

    static func card(_ id: String) throws -> ViewDocument {
        let names = [questionID: "card-question", informationID: "card-information",
                     deliveryID: "card-delivery", pullRequestID: "card-pr"]
        return try document(try XCTUnwrap(names[id]))
    }

    /// The home view with its queue replaced, as the daemon sends it when something moved.
    static func home(queue: [String], version: String = "vq", focus: String? = nil, spanish: Bool = false) throws -> ViewDocument {
        let data = try Data(contentsOf: directory().appendingPathComponent(spanish ? "home-ios-es.json" : "home-ios.json"))
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        object["version"] = version
        if let focus { object["key"] = "home?focus=\(focus)" }
        var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
        sections[0]["card_ids"] = queue
        sections[0]["card_waits"] = queue.map { [informationID, deliveryID, pullRequestID].contains($0) ? "read" : "question" }
        object["sections"] = sections
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }

    /// A fixture view with one JSON edit applied, for the shapes the daemon has not rendered yet.
    static func document(_ name: String, edit: (inout [String: Any]) throws -> Void) throws -> ViewDocument {
        let data = try Data(contentsOf: directory().appendingPathComponent("\(name).json"))
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        try edit(&object)
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }

    /// The card view of `id` as the daemon serves it to a Spanish phone, with Spanish node text.
    static func spanishCard(_ id: String) throws -> ViewDocument {
        let names = [questionID: "card-question-es", informationID: "card-information-es",
                     deliveryID: "card-delivery-es", pullRequestID: "card-pr-es"]
        return try spanish(try document(try XCTUnwrap(names[id])))
    }

    static func spanish(_ document: ViewDocument) throws -> ViewDocument {
        try SpanishTestData.document(document)
    }

    /// The first card of a card view, edited.
    static func card(_ name: String, edit: (inout [String: Any]) throws -> Void) throws -> ViewDocument {
        try document(name) { object in
            var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
            var items = try XCTUnwrap(sections[0]["items"] as? [[String: Any]])
            try edit(&items[0])
            sections[0]["items"] = items
            object["sections"] = sections
        }
    }
}
