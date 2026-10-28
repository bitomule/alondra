import AlondraContract
import ComposableArchitecture
import Foundation

/// Building, reading and sending a crash report on the Mac this app talks to. Nothing is sent
/// until `send`.
public struct CrashReportClient: Sendable {
    /// Builds a new report with what the user wrote (sent as written) and the app's own section.
    public var build: @Sendable (_ description: String?, _ failure: String, _ appSection: String) async throws -> CrashReport
    /// One file of the report, whole, exactly as it would leave the Mac.
    public var file: @Sendable (_ id: String, _ name: String) async throws -> String
    public var send: @Sendable (_ id: String, _ failure: String) async throws -> CrashReportSent
    /// `apps.jsonl`: this app, its connection, its redacted log lines and its connection measurements.
    public var appSection: @Sendable () async -> String
    public var problem: @Sendable (any Error) -> ConnectionProblem
    /// The Mac is the one this app runs on, so what to do when it does not answer is said for it.
    public var onThisMac: Bool

    public init(build: @escaping @Sendable (String?, String, String) async throws -> CrashReport,
                file: @escaping @Sendable (String, String) async throws -> String,
                send: @escaping @Sendable (String, String) async throws -> CrashReportSent,
                appSection: @escaping @Sendable () async -> String = { "" },
                problem: @escaping @Sendable (any Error) -> ConnectionProblem = { _ in .unexpected },
                onThisMac: Bool = false) {
        self.build = build; self.file = file; self.send = send; self.appSection = appSection
        self.problem = problem; self.onThisMac = onThisMac
    }
}

/// The Mac answered something that is not what was asked for.
public struct CrashReportUnreadable: Error, Equatable {
    public init() {}
}

extension CrashReportClient: DependencyKey {
    public static let liveValue = Self(build: { _, _, _ in throw CancellationError() },
                                       file: { _, _ in throw CancellationError() },
                                       send: { _, _ in throw CancellationError() })
}

public extension DependencyValues {
    var crashReports: CrashReportClient {
        get { self[CrashReportClient.self] }
        set { self[CrashReportClient.self] = newValue }
    }
}
