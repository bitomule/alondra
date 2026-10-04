import AlondraContract
import Foundation
import XCTest

/// Cards the daemon renders that the shared fixtures do not hold: each one is a golden card with
/// the fields its kind changes edited, so it keeps the shape the daemon sends.
extension Fixtures {
    static func button(
        _ id: String, _ label: String, role: String = "neutral", optionID: String? = nil,
        input: [String: Any] = ["kind": "none"], enabled: Bool = true, stamp: String = "Answered",
        extra: [String: Any] = [:]
    ) -> [String: Any] {
        var value: [String: Any] = [
            "id": id, "label": label, "role": role, "presentation": "button", "input": input, "attachments": false,
            "enabled": enabled, "stamp": stamp,
            "style": role == "advance" ? "primary" : role == "risk" ? "destructive" : "secondary",
        ]
        if let optionID { value["option_id"] = optionID }
        for (key, entry) in extra { value[key] = entry }
        return value
    }

    /// The first card of a golden card view, given a new id, kind and wording.
    static func retargeted(
        _ name: String = "card-question", id: String, kind: String, need: String, why: String,
        content: String? = nil, edit: (inout [String: Any]) throws -> Void = { _ in }
    ) throws -> ViewDocument {
        let document = try card(name) { item in
            item["id"] = id
            item["link"] = ["kind": "view", "key": "card:\(id)"]
            item["kind"] = kind
            item["need"] = need
            item["title"] = need
            item["why"] = why
            item["actions"] = []
            item["more"] = nil
            try edit(&item)
        }
        .rekeyed("card:\(id)")
        guard let content else { return document }
        return try document.replacingMarkdown(of: "content", with: content)
    }

    /// Permission for a command Claude Code held: Yes advances, Always and No sit under it.
    static func authorizeCard(id: String = "perm-1") throws -> ViewDocument {
        try retargeted(
            id: id, kind: "authorize", need: "Allow publishing Brew 0.2.36",
            why: "It restarts the daemon for a few seconds. You can go back to 0.2.35.",
            content: "```\nalondra update --channel brew\n```"
        ) { item in
            item["state"] = ["text": "Blocks release", "tone": "warning"]
            item["content_row"] = ["kind": "command", "icon": "warning", "label": "Command", "summary": "alondra update"]
            item["actions"] = [
                button("ans:\(id)@1", "Yes", role: "advance", optionID: "allow"),
                button("ans:\(id)@1", "Always for this", optionID: "always"),
                button("ans:\(id)@1", "No", optionID: "deny"),
            ]
        }
    }

    /// A Problem: the daemon raised it and the way out is a risky button.
    static func problemCard(id: String = "problem-1") throws -> ViewDocument {
        try retargeted(
            id: id, kind: "problem", need: "Retry the failed scheduled launch",
            why: "Release at 09:00 could not start: the project folder is missing.",
            content: "The project folder `~/Projects/release` does not exist on this Mac."
        ) { item in
            item["state"] = ["text": "Blocks the schedule", "tone": "warning"]
            item["content_row"] = ["kind": "diagnosis", "icon": "warning", "label": "Diagnosis", "summary": "folder not found"]
            item["actions"] = [
                button("ans:\(id)@1", "Retry now", role: "risk", optionID: "retry"),
                button("ans:\(id)@1", "Skip this run", optionID: "skip"),
            ]
        }
    }

    /// A decision with five options: three buttons show and two wait under "More…".
    static func crowdedDecision(id: String = "decide-5") throws -> ViewDocument {
        try retargeted(
            id: id, kind: "decide", need: "Pick the release channel", why: "Five channels are open and one ships first."
        ) { item in
            item["content_row"] = ["kind": "options", "icon": "notice", "label": "Why each option", "summary": "5 options"]
            item["actions"] = ["Stable", "Beta", "Nightly", "Preview", "Internal"].enumerated().map { index, name in
                button("ans:\(id)@1", index == 0 ? "★ \(name)" : name, role: index == 0 ? "advance" : "neutral",
                       optionID: name.lowercased())
            }
            item["more"] = [
                button("ans:\(id)@1", "Write my own answer", input: ["kind": "text", "placeholder": "Answer in your own words…",
                                                                    "required": true, "multiline": true]),
                clarify(id: id),
            ]
        }
    }

    /// "Request clarification": which reason applies, and an optional word on what to clarify.
    static func clarify(id: String) -> [String: Any] {
        var value = button(
            "clarify:\(id)@1", "Request clarification",
            input: ["kind": "text", "placeholder": "What should it clarify?", "required": false, "multiline": true], stamp: "Clarification requested"
        )
        value["presentation"] = "options"
        value["options"] = [
            ["id": "unclear", "label": "It is unclear", "style": "secondary", "input": ["kind": "none"]],
            ["id": "contradicts", "label": "It contradicts something", "style": "secondary", "input": ["kind": "none"]],
            ["id": "missing_context", "label": "It lacks context", "style": "secondary", "input": ["kind": "none"]],
        ]
        return value
    }

    /// A plan to approve: the text behind a content reference the detail fetches and can comment on.
    static func planCard(id: String = "plan-1") throws -> ViewDocument {
        let document = try retargeted(
            "card-delivery", id: id, kind: "review", need: "Approve the compact schedule plan",
            why: "Two files change and nothing starts until you approve."
        ) { item in
            item["content_row"] = ["kind": "plan", "icon": "document", "label": "Plan", "summary": "6 steps"]
            item["blocked_by"] = nil
            item["actions"] = [
                button("accept:\(id)@1", "Approve", role: "advance", stamp: "Approved"),
                button("changes:\(id)@1", "Request changes", input: ["kind": "text", "placeholder": "Describe the changes",
                                                                    "required": true, "multiline": true],
                       stamp: "Changes requested", extra: ["attachments": true, "comments": ["kind": "blocks"]]),
            ]
            item["more"] = [clarify(id: id)]
        }
        return try document.replacingSections([
            ["id": "card", "collapsed": false, "items": try document.itemsJSON(of: "card")],
            ["id": "content", "title": "Content", "collapsed": false, "items": [[
                "type": "content_ref", "kind": "document", "title": "Plan", "size": 0, "digest": "plan-digest",
                "fetch": "/document-revisions/\(id)/content", "commentable": true,
            ]]],
        ])
    }

    /// A plan Claude Code holds, as the daemon sends it: the text rides inside the card, commentable.
    static func heldPlanCard(id: String = "plan-1", text: String) throws -> ViewDocument {
        let document = try planCard(id: id)
        return try document.replacingSections([
            ["id": "card", "collapsed": false, "items": try document.itemsJSON(of: "card")],
            ["id": "content", "title": "Content", "collapsed": false, "items": [[
                "type": "markdown", "text": text, "truncated": false, "commentable": true,
            ]]],
        ])
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
                        button("accept:rev-1@1", "Approve", role: "advance", stamp: "Approved"),
                        button("changes:rev-1@1", "Request changes", extra: ["comments": ["kind": "blocks"]]),
                    ],
                ]]],
            ]
        }
    }

    /// The same card with every button live, as the daemon sends it once what blocked it is answered.
    static func unblocked(_ document: ViewDocument) throws -> ViewDocument {
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: ContractCodec.encode(document)) as? [String: Any])
        var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
        var items = try XCTUnwrap(sections[0]["items"] as? [[String: Any]])
        for key in ["actions", "more"] {
            var actions = items[0][key] as? [[String: Any]] ?? []
            for index in actions.indices {
                actions[index]["enabled"] = true
                actions[index]["disabled_reason"] = nil
            }
            items[0][key] = actions
        }
        items[0]["blocked_by"] = nil
        sections[0]["items"] = items
        object["sections"] = sections
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }
}

extension ViewDocument {
    fileprivate func json() throws -> [String: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: ContractCodec.encode(self)) as? [String: Any])
    }

    func itemsJSON(of section: String) throws -> [[String: Any]] {
        let sections = try XCTUnwrap(json()["sections"] as? [[String: Any]])
        return try XCTUnwrap(sections.first { $0["id"] as? String == section }?["items"] as? [[String: Any]])
    }

    func rekeyed(_ key: String) throws -> ViewDocument {
        var object = try json()
        object["key"] = key
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }

    func replacingMarkdown(of section: String, with text: String) throws -> ViewDocument {
        var object = try json()
        var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
        let index = try XCTUnwrap(sections.firstIndex { $0["id"] as? String == section })
        var items = try XCTUnwrap(sections[index]["items"] as? [[String: Any]])
        let item = try XCTUnwrap(items.firstIndex { $0["type"] as? String == "markdown" })
        items[item]["text"] = text
        sections[index]["items"] = items
        object["sections"] = sections
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }

    /// The same detail with what has happened to the card so far.
    func withHistory() throws -> ViewDocument {
        var object = try json()
        var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
        sections.append(["id": "history", "title": "History", "collapsed": false, "items": [
            ["type": "row", "id": "clarification", "icon": "history", "title": "You asked for clarification",
             "subtitle": "The two options say opposite things", "depth": 0, "struck": false],
            ["type": "row", "id": "answer", "icon": "check", "title": "Answered · Ship the concise copy",
             "depth": 0, "struck": false],
        ]])
        object["sections"] = sections
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }

    func replacingSections(_ sections: [[String: Any]]) throws -> ViewDocument {
        var object = try json()
        object["sections"] = sections
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }
}
