import AlondraShared
import CryptoKit
import Foundation

enum DemoFixture {
    /// The same built-in catalogue served by core/options.rs, plus a sample project option.
    static func options(project: String?) -> [String: Any] {
        var specs: [[String: Any]] = [
            ["id": "pr", "label": "PR handling", "kind": "choice", "default": "review", "builtin": true,
             "values": [
                ["id": "review", "label": "review each PR", "description": "Each PR the node opens is a card for you; the node waits for your merge"],
                ["id": "auto_merge", "label": "auto-merge", "description": "The node merges its own PR once its checks pass"]]],
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
    static var documentDigest: String {
        SHA256.hash(data: Data(document.utf8)).map { String(format: "%02x", $0) }.joined()
    }
    static let diff = """
    diff --git a/Welcome.swift b/Welcome.swift
    --- a/Welcome.swift
    +++ b/Welcome.swift
    @@ -1 +1 @@
    -let greeting = "Welcome to the dashboard"
    +let greeting = "What needs your attention?"
    """
}
