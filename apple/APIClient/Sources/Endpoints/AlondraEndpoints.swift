import AlondraContracts
import Foundation

public enum AlondraEndpoints {
    public static func overview() -> Endpoint<Data> { .data(.get, path: ["v1", "overview"]) }
    public static func task(_ id: String) -> Endpoint<Data> { .data(.get, path: ["v1", "tasks", id]) }
    public static func nodeActivity(_ session: String) -> Endpoint<Data> { .data(.get, path: ["v1", "nodes", session, "activity"]) }
    public static func pullRequestDiff(_ id: UInt64) -> Endpoint<Data> { .data(.get, path: ["v1", "prs", String(id), "diff"]) }
    public static func schedules() -> Endpoint<Data> { .data(.get, path: ["v1", "schedules"]) }
    public static func options(project: String?) -> Endpoint<Data> {
        .data(.get, path: ["v1", "options"], query: project.map { [("project", $0)] } ?? [])
    }
    public static func issues(refresh: Bool) -> Endpoint<Data> {
        .data(.get, path: ["v1", "issues"], query: refresh ? [("refresh", "1")] : [])
    }
    public static func projects() -> Endpoint<Data> { .data(.get, path: ["v1", "projects"]) }
    public static func search(_ query: String) -> Endpoint<Data> { .data(.get, path: ["v1", "search"], query: [("q", query)]) }
    public static func fileText(_ path: String) -> Endpoint<Data> { .data(.get, path: ["v1", "file"], query: [("path", path)]) }

    public static func answer(_ question: UInt64, text: String, option: String?, attachments: [String], device: String) throws -> Endpoint<Data> {
        var body: [String: Any] = ["text": text, "device": device, "attachments": attachments]
        body["option"] = option
        return try json(.post, ["v1", "questions", String(question), "answer"], body)
    }
    public static func moreContext(_ question: UInt64, note: String, attachments: [String]) throws -> Endpoint<Data> {
        try json(.post, ["v1", "questions", String(question), "more-context"], ["note": note, "attachments": attachments])
    }
    public static func mergePullRequest(_ id: UInt64, method: String) throws -> Endpoint<Data> {
        try json(.post, ["v1", "prs", String(id), "merge"], ["method": method])
    }
    public static func requestChanges(_ id: UInt64, text: String, attachments: [String]) throws -> Endpoint<Data> {
        try json(.post, ["v1", "prs", String(id), "changes"], ["text": text, "attachments": attachments])
    }
    public static func ask(_ id: UInt64, text: String, attachments: [String]) throws -> Endpoint<Data> {
        try json(.post, ["v1", "prs", String(id), "ask"], ["text": text, "attachments": attachments])
    }
    public static func resolveConflicts(_ id: UInt64) throws -> Endpoint<Data> { try json(.post, ["v1", "prs", String(id), "resolve-conflicts"], [:]) }
    public static func dismissPullRequest(_ id: UInt64) throws -> Endpoint<Data> { try json(.post, ["v1", "prs", String(id), "dismiss"], [:]) }
    public static func cancelTask(_ id: String) throws -> Endpoint<Data> { try json(.post, ["v1", "tasks", id, "cancel"], [:]) }
    public static func reopenTask(_ id: String) throws -> Endpoint<Data> { try json(.post, ["v1", "tasks", id, "reopen"], [:]) }
    public static func withdrawRequest(_ id: String) throws -> Endpoint<Data> { try json(.post, ["v1", "requests", id, "close"], ["withdrawn": true]) }
    public static func archiveCompletedTasks(ids: [String]?) throws -> Endpoint<Data> {
        try json(.post, ["v1", "tasks", "archive"], ids.map { ["ids": $0] } ?? [:])
    }
    public static func activity(client: String) throws -> Endpoint<Data> { try json(.post, ["v1", "activity"], ["client": client]) }
    public static func newRequest(_ body: NewRequestBody) throws -> Endpoint<Data> {
        .data(.post, path: ["v1", "requests"], body: try JSONEncoder().encode(body), contentType: "application/json")
    }
    public static func saveSchedule(_ body: Data, id: String?) -> Endpoint<Data> {
        .data(id == nil ? .post : .put, path: ["v1", "schedules"] + (id.map { [$0] } ?? []), body: body, contentType: "application/json")
    }
    public static func deleteSchedule(_ id: String) -> Endpoint<Data> { .data(.delete, path: ["v1", "schedules", id]) }
    public static func runSchedule(_ id: String) -> Endpoint<Data> {
        .data(.post, path: ["v1", "schedules", id, "run"], body: Data("{}".utf8), contentType: "application/json")
    }
    public static func markRead(_ report: UInt64) throws -> Endpoint<Data> { try json(.post, ["v1", "reports", String(report), "read"], [:]) }
    public static func review(_ report: UInt64, verdict: String) throws -> Endpoint<Data> {
        try json(.post, ["v1", "reports", String(report), "review"], ["verdict": verdict])
    }
    public static func closeNode(_ key: String, force: Bool) -> Endpoint<Data> {
        .data(.post, path: ["v1", "nodes", key, "close"], body: Data((force ? #"{"force":true}"# : "{}").utf8), contentType: "application/json")
    }
    public static func send(to session: String, text: String, attachments: [String]) throws -> Endpoint<Data> {
        try json(.post, ["v1", "nodes", session, "messages"], ["text": text, "from": "david", "attachments": attachments])
    }

    private static func json(_ method: HTTPMethod, _ path: [String], _ object: [String: Any]) throws -> Endpoint<Data> {
        .data(method, path: path, body: try JSONSerialization.data(withJSONObject: object), contentType: "application/json")
    }
}
