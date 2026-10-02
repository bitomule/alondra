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
    static func home(queue: [String], version: String = "vq", focus: String? = nil) throws -> ViewDocument {
        let data = try Data(contentsOf: directory().appendingPathComponent("home-ios.json"))
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        object["version"] = version
        if let focus { object["key"] = "home?focus=\(focus)" }
        var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
        sections[0]["card_ids"] = queue
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

    /// A document revision as `document:{id}` serves it: the text arrives by digest.
    static func documentRevision(digest: String = "digest-1") throws -> ViewDocument {
        try document("card-delivery") { object in
            object["view"] = "document"
            object["key"] = "document:rev-1"
            object["title"] = "Release plan"
            object["sections"] = [
                ["id": "body", "collapsed": false, "items": [[
                    "type": "content_ref", "kind": "document", "title": "Release plan", "size": 0,
                    "digest": digest, "fetch": "/document-revisions/rev-1/content", "commentable": true,
                ]]],
                ["id": "actions", "collapsed": false, "items": [[
                    "type": "actions", "actions": [
                        ["id": "accept:rev-1@1", "label": "Approve", "style": "primary", "presentation": "button",
                         "input": ["kind": "none"], "attachments": false, "enabled": true],
                        ["id": "changes:rev-1@1", "label": "Request changes", "style": "secondary",
                         "presentation": "button", "input": ["kind": "none"], "attachments": false,
                         "enabled": true, "comments": ["kind": "blocks"]],
                    ],
                ]]],
            ]
        }
    }

    /// A delivery with no Claude artifact, as the daemon renders it while its actions are live: a reply field and Accept.
    static func livePlainDelivery() throws -> ViewDocument {
        try card("card-delivery") { item in
            var actions = try XCTUnwrap(item["actions"] as? [[String: Any]])
            for index in actions.indices {
                actions[index]["enabled"] = true
                actions[index]["disabled_reason"] = nil
            }
            item["actions"] = actions
        }
    }

    /// A delivery that links a Claude artifact, as the daemon renders it while its actions are live: Rescue Comments and Approve.
    static func liveDelivery() throws -> ViewDocument {
        try card("card-delivery") { item in
            var actions = try XCTUnwrap(item["actions"] as? [[String: Any]])
            for index in actions.indices {
                actions[index]["enabled"] = true
                actions[index]["disabled_reason"] = nil
            }
            asArtifactReview(&actions)
            item["actions"] = actions
            var body = try XCTUnwrap(item["body"] as? [String: Any])
            body["text"] = "\(try XCTUnwrap(body["text"] as? String))\n\nhttps://claude.ai/artifact/abc123"
            item["body"] = body
        }
    }

    /// The feedback action a delivery with an artifact or a document carries: request changes with block comments.
    static func asArtifactReview(_ actions: inout [[String: Any]]) {
        actions.append([
            "id": "changes:\(deliveryID)@1", "label": "Request changes", "style": "secondary", "presentation": "button",
            "input": ["kind": "text", "placeholder": "Describe the changes", "required": true, "multiline": true],
            "attachments": true, "comments": ["kind": "blocks"], "enabled": true, "stamp": "Changes requested",
        ])
    }

    /// A tool-permission prompt: the command in a code block, Allow and Deny to pick from.
    static func permissionCard() throws -> ViewDocument {
        try document("card-question") { object in
            object["key"] = "card:perm-1"
            var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
            var items = try XCTUnwrap(sections[0]["items"] as? [[String: Any]])
            items[0]["id"] = "perm-1"
            items[0]["link"] = ["kind": "view", "key": "card:perm-1"]
            items[0]["title"] = "Run a command?"
            items[0]["body"] = ["text": "```\nnpm test -- --watch=false\n```", "truncated": false, "commentable": false]
            func option(_ id: String, _ label: String, _ style: String) -> [String: Any] {
                ["id": id, "label": label, "style": style, "input": ["kind": "none"]]
            }
            items[0]["actions"] = [[
                "id": "ans:perm-1@1", "label": "Answer", "style": "primary", "presentation": "options",
                "options": [option("allow", "Allow", "primary"), option("deny", "Deny", "secondary")],
                "input": ["kind": "none"], "attachments": false, "enabled": true, "stamp": "Answered",
            ]]
            sections[0]["items"] = items
            object["sections"] = sections
        }
    }

    /// A plan to approve or open for review: a document card with its text behind a content ref.
    static func planCard() throws -> ViewDocument {
        try document("card-delivery") { object in
            object["key"] = "card:plan-1"
            var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
            var items = try XCTUnwrap(sections[0]["items"] as? [[String: Any]])
            items[0]["id"] = "plan-1"
            items[0]["icon"] = "document"
            items[0]["link"] = ["kind": "view", "key": "card:plan-1"]
            items[0]["title"] = "Plan: compact schedule editor"
            items[0]["body"] = ["text": "## Plan\n\n1. Build the form\n2. Wire the sheet", "truncated": true,
                                "commentable": true]
            func action(_ id: String, _ label: String, _ style: String, comments: Bool) -> [String: Any] {
                var value: [String: Any] = [
                    "id": id, "label": label, "style": style, "presentation": "button",
                    "input": ["kind": "none"], "attachments": false, "enabled": true,
                ]
                if comments { value["comments"] = ["kind": "blocks"] }
                return value
            }
            items[0]["actions"] = [
                action("accept:plan-1@1", "Approve", "primary", comments: false),
                action("changes:plan-1@1", "Review", "secondary", comments: true),
            ]
            sections[0]["items"] = items
            object["sections"] = sections
        }
    }
}
