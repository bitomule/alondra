import AlondraTransport
import Foundation

/// Why `POST /v1/nodes/{key}/close` refused, in the user's language and without the server's
/// status code or raw prose. `detail`, when present, names the worktree or files at stake and
/// is not itself translated.
func closeReasonMessage(kind: String, detail: String?) -> String {
    switch kind {
    case "busy":
        return String(localized: "it is working; wait until it stops", table: "AlondraDesign", bundle: #bundle)
    case "permission":
        return String(localized: "it has a permission pending on a tool call that is running; answer it and close again", table: "AlondraDesign", bundle: #bundle)
    case "orchestrator":
        return String(localized: "it is the orchestrator, which Alondra manages automatically", table: "AlondraDesign", bundle: #bundle)
    case "worktree":
        return String(localized: "its worktree has work that is not pushed (\(detail ?? "")); commit and push it, or discard it, and close again", table: "AlondraDesign", bundle: #bundle)
    default:
        return detail ?? kind
    }
}

public enum AlondraError: LocalizedError, Equatable {
    /// alondrad's own JSON error, meant for the user.
    case server(String)
    case unauthorized
    /// A gateway answered instead of alondrad (the tunnel is up, alondrad is not): worth retrying.
    case unreachable
    /// Any other failure whose body is not alondrad's JSON; the body only goes to the log.
    case unexpected(Int)

    public static let longestMessage = 300

    /// Keep the app's existing localized error contract while the transport reports typed status.
    public static func from(_ error: APIError) -> AlondraError {
        switch error {
        case .unauthorized: return .unauthorized
        case .unavailable: return .unreachable
        case .unexpected(let status): return .unexpected(status)
        case .forbidden(let kind, let detail, let reasons):
            return message(status: 403, kind: kind, detail: detail, reasons: reasons)
        case .conflict(let kind, let detail, let reasons):
            return message(status: 409, kind: kind, detail: detail, reasons: reasons)
        case .server(let status, let kind, let detail, let reasons):
            return message(status: status, kind: kind, detail: detail, reasons: reasons)
        }
    }

    private static func message(status: Int, kind: String?, detail: String?, reasons: [APIError.RefusalReason]) -> AlondraError {
        if !reasons.isEmpty {
            let lines = reasons.map { closeReasonMessage(kind: $0.kind, detail: $0.detail) }
            return .server(lines.count == 1 ? lines[0] : lines.map { "- \($0)" }.joined(separator: "\n"))
        }
        if status == 404 {
            return .server(String(localized: "Alondra no longer has it", table: "AlondraDesign", bundle: #bundle))
        }
        if let detail, !detail.isEmpty { return .server(refusalMessage(kind: kind, detail: detail)) }
        return .unexpected(status)
    }

    /// Only alondrad's JSON errors reach the user as text; a proxy's HTML page never does.
    /// A 5xx carrying alondrad's own JSON is alondrad answering, not a gateway standing in for it:
    /// sending the same thing again would get the same answer.
    public static func from(status: Int, body: Data) -> AlondraError {
        if status == 401 { return .unauthorized }
        let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any]
        if let reasons = json?["reasons"] as? [[String: Any]], !reasons.isEmpty {
            let lines = reasons.map { closeReasonMessage(kind: $0["kind"] as? String ?? "", detail: $0["detail"] as? String) }
            return .server(lines.count == 1 ? lines[0] : lines.map { "- \($0)" }.joined(separator: "\n"))
        }
        if status == 404, json?["error"] != nil {
            return .server(String(localized: "Alondra no longer has it", table: "AlondraDesign", bundle: #bundle))
        }
        if let message = (json?["error"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines), !message.isEmpty {
            let detail = message.count > longestMessage ? String(message.prefix(longestMessage)) + "…" : message
            return .server(refusalMessage(kind: json?["kind"] as? String, detail: detail))
        }
        if [502, 503, 504].contains(status) { return .unreachable }
        return .unexpected(status)
    }

    /// alondrad's `kind` in the user's language; its English `error` only when the kind is unknown.
    static func refusalMessage(kind: String?, detail: String) -> String {
        switch kind {
        case "pr_not_open": String(localized: "This PR is no longer open", table: "AlondraDesign", bundle: #bundle)
        case "merge_refused": String(localized: "GitHub didn’t merge it: \(detail)", table: "AlondraDesign", bundle: #bundle)
        case "question_closed": String(localized: "This question was already answered", table: "AlondraDesign", bundle: #bundle)
        case "task_refused": String(localized: "The task’s state does not allow that now", table: "AlondraDesign", bundle: #bundle)
        default: detail
        }
    }

    public var errorDescription: String? {
        switch self {
        case .server(let message): message
        case .unauthorized: String(localized: "This device is not paired", table: "AlondraDesign", bundle: #bundle)
        case .unreachable: String(localized: "Alondra isn’t responding right now", table: "AlondraDesign", bundle: #bundle)
        case .unexpected(let status): String(localized: "The server responded \(status)", table: "AlondraDesign", bundle: #bundle)
        }
    }
}

extension Error {
    public var isCancellation: Bool {
        if self is CancellationError { return true }
        return (self as? URLError)?.code == .cancelled
    }

    /// A failure that says nothing about the request itself (alondrad down, restarting, the
    /// network gone): sending the same thing again later is the right answer.
    public var isTransient: Bool {
        if let error = self as? AlondraError { return error == .unreachable }
        guard let code = (self as? URLError)?.code else { return false }
        return [.timedOut, .cannotConnectToHost, .cannotFindHost, .networkConnectionLost, .notConnectedToInternet, .dnsLookupFailed].contains(code)
    }

    public var friendlyDescription: String {
        guard let urlError = self as? URLError else { return localizedDescription }
        switch urlError.code {
        case .cannotConnectToHost, .cannotFindHost: return String(localized: "Can’t find the Alondra server", table: "AlondraDesign", bundle: #bundle)
        case .notConnectedToInternet, .networkConnectionLost: return String(localized: "No connection", table: "AlondraDesign", bundle: #bundle)
        case .timedOut: return String(localized: "The server isn’t responding", table: "AlondraDesign", bundle: #bundle)
        default: return urlError.localizedDescription
        }
    }
}
