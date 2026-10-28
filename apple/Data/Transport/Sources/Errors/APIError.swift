import Foundation

public enum APIError: Error, Equatable, Sendable {
    case unauthorized
    case forbidden(kind: String?, detail: String?, reasons: [RefusalReason])
    case conflict(kind: String?, detail: String?, reasons: [RefusalReason])
    case server(status: Int, kind: String?, detail: String?, reasons: [RefusalReason])
    case unavailable
    case unexpected(status: Int)

    public struct RefusalReason: Equatable, Sendable {
        public let kind: String
        public let detail: String?

        public init(kind: String, detail: String?) {
            self.kind = kind
            self.detail = detail
        }
    }

    public static func from(status: Int, body: Data) -> APIError {
        if status == 401 { return .unauthorized }
        let object = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
        let kind = object?["kind"] as? String
        let detail = (object?["error"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmed = detail.map { $0.count > 300 ? String($0.prefix(300)) + "…" : $0 }
        let reasons = (object?["reasons"] as? [[String: Any]] ?? []).map {
            RefusalReason(kind: $0["kind"] as? String ?? "", detail: $0["detail"] as? String)
        }
        if status == 422, let message = object?["message"] as? String, !message.isEmpty {
            return .server(status: status, kind: object?["error"] as? String, detail: message, reasons: reasons)
        }
        let message = (object?["message"] as? String).flatMap { $0.isEmpty ? nil : $0 }
        if status == 403 { return .forbidden(kind: kind, detail: trimmed, reasons: reasons) }
        if status == 409 { return .conflict(kind: kind, detail: trimmed ?? message, reasons: reasons) }
        if status == 404, object?["error"] != nil || message != nil {
            return .server(status: status, kind: kind, detail: trimmed ?? message, reasons: reasons)
        }
        if !reasons.isEmpty || (trimmed?.isEmpty == false) {
            return .server(status: status, kind: kind, detail: trimmed, reasons: reasons)
        }
        if [502, 503, 504].contains(status) { return .unavailable }
        return .unexpected(status: status)
    }
}
