import Foundation

/// What sending a report came to, in the Mac's words: sent with its issue, pending until GitHub
/// answers, or refused with why.
public struct CrashReportSent: Equatable, Sendable {
    public enum State: String, Equatable, Sendable { case sent, pending, refused }

    public var state: State
    public var url: URL?
    public var message: String

    public init(state: State, url: URL? = nil, message: String) {
        self.state = state; self.url = url; self.message = message
    }

    public init?(daemonReply data: Data) {
        guard let answer = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let state = (answer["state"] as? String).flatMap(State.init(rawValue:)) else { return nil }
        let url = (answer["url"] as? String).flatMap(URL.init(string:)).flatMap { $0.scheme == "https" ? $0 : nil }
        self.init(state: state, url: url, message: answer["message"] as? String ?? "")
    }
}
