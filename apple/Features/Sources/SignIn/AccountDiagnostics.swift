import AlondraContract
import OSLog
import Synchronization

/// The account states the person sees, written to the system log at notice level (kept on disk)
/// so a lost session can be explained afterwards. Lines carry error kinds only, never secrets.
public enum AccountDiagnostics {
    public typealias Sink = @Sendable (_ line: String, _ failed: Bool) -> Void

    private static let log = Logger(subsystem: "com.bitomule.alondra", category: "auth")
    public static let systemLog: Sink = { line, failed in
        if failed { log.error("\(line, privacy: .public)") }
        else { log.notice("\(line, privacy: .public)") }
    }
    private static let sink = Mutex<Sink>(systemLog)

    /// Tests replace where the lines go.
    public static func record(to newSink: @escaping Sink) { sink.withLock { $0 = newSink } }

    public static func personSees(_ problem: ConnectionProblem, after cause: String) {
        let current = sink.withLock { $0 }
        current("person sees \(problem) after \(cause)", problem == .sessionExpired)
    }
}
