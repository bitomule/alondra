import AlondraShared
import Foundation

enum DemoFixture {
    /// The same built-in catalogue served by core/options.rs, plus a sample project option.
    static func options(project: String?) -> [String: Any] {
        var specs: [[String: Any]] = [
            ["id": "pr", "label": "PR handling", "kind": "choice", "default": "review", "builtin": true,
             "values": [
                ["id": "review", "label": "review each PR", "description": "Each PR the node opens is a card for you; the node waits for your merge"],
                ["id": "auto_merge", "label": "auto-merge", "description": "Alondra merges the node's PR once its checks pass"]]],
            ["id": "effort", "label": "Effort", "kind": "choice", "default": "auto", "builtin": true,
             "values": [
                ["id": "auto", "label": "auto", "description": "The orchestrator picks the model and effort from the task"],
                ["id": "light", "label": "light", "description": "A quick, cheaper model for small, clear work"],
                ["id": "normal", "label": "normal", "description": "Claude's default model and effort"],
                ["id": "hard", "label": "hard", "description": "The strongest model and the most effort"]]]
        ]
        if project == "Demo Garden" {
            specs.append(["id": "screenshots", "label": "Include screenshots", "kind": "toggle", "default": true, "builtin": false])
        }
        var catalogue: [String: Any] = ["options": specs]
        if let project { catalogue["project"] = project }
        return catalogue
    }

    static func make(now: Int64) -> [String: Any] {
        let question: [String: Any] = ["id": 1, "sessionId": "demo-design", "nodeName": "Design assistant",
            "title": "Which welcome screen should we use?", "detail": "Choose the calmer layout or keep the current design.",
            "options": [["label": "Calmer layout"], ["label": "Keep current"]], "links": [], "attachments": [],
            "origin": "ask", "status": "open", "askedAt": now, "task": "demo-task"]
        let report: [String: Any] = ["id": 2, "sessionId": "demo-design", "nodeName": "Design assistant",
            "title": "Welcome screen proposal", "body": "A short plan for the welcome screen. Review the sample document and leave feedback.",
            "links": [["title": "Sample design", "url": "demo://welcome.md"]], "attachments": [["name": "welcome.md", "sourcePath": "demo://welcome.md", "storedPath": "demo://welcome.md"]],
            "kind": "document", "createdAt": now, "task": "demo-task"]
        let task: [String: Any] = ["id": "demo-task", "request": "demo-request", "title": "Polish the welcome experience",
            "brief": "Explore the example question, document and pull request.", "dependsOn": [], "prs": [3], "documents": [2],
            "notices": [], "state": "needs_you", "stateSince": now, "createdAt": now,
            "assignee": ["sessionId": "demo-design"]]
        return ["tick": 1,
            "nodes": [["sessionId": "demo-design", "shortId": "design", "name": "Design assistant", "project": "Demo Garden",
                       "isOrchestrator": true, "hasPlugin": true, "state": "waiting_answer", "stateSince": now,
                       "stateSource": "demo", "lastMessage": "Waiting for your feedback on the example work."]],
            "openQuestions": [question], "news": [report], "reports": [report], "orchestratorMessages": [],
            "machine": ["memoryTotal": 16_000_000_000, "memoryAvailable": 10_000_000_000, "load": [0.2, 0.3, 0.2],
                        "diskAvailable": 120_000_000_000, "unreliable": false], "user": "Demo",
            "prs": [["id": 3, "url": "demo://pull-request/3", "repo": "demo/garden", "number": 3,
                     "sessionId": "demo-design", "nodeName": "Design assistant", "title": "Simplify the welcome message",
                     "body": "Example pull request. Every action changes only this demo.", "base": "main", "head": "demo/welcome",
                     "state": "open", "mergeable": "MERGEABLE", "checks": [["name": "Example integration checks", "state": "passed"]],
                     "mergeMethods": ["squash"], "additions": 1, "deletions": 1, "changedFiles": 1, "openedAt": now, "task": "demo-task"]],
            "requests": [["id": "demo-request", "text": "Make the welcome experience simpler", "title": "Welcome experience",
                          "createdAt": now, "state": "in_progress", "tasks": ["demo-task"]]], "tasks": [task]]
    }
    static func decode<T: Decodable>(_ value: Any, as: T.Type = T.self) throws -> T {
        try JSONDecoder().decode(T.self, from: JSONSerialization.data(withJSONObject: value))
    }
    static let document = """
    # Welcome experience

    This is a synthetic document stored only in memory.

    ## Proposal

    - Lead with the next useful action.
    - Keep the welcome message short.
    - Preserve readable contrast and accessible controls.

    Select a line or add a general note to try reviewing a document.
    """
    static let diff = """
    diff --git a/Welcome.swift b/Welcome.swift
    --- a/Welcome.swift
    +++ b/Welcome.swift
    @@ -1 +1 @@
    -let greeting = "Welcome to the dashboard"
    +let greeting = "What needs your attention?"
    """
}
