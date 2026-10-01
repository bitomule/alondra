import Foundation

/// A scene's action must revalidate its captured session when it executes, not in a
/// separate preflight actor hop. Operations must also validate after their own awaits.
@MainActor
public enum SessionBoundAction {
    public static func run<Session: AnyObject, Result>(expected: Session,
        current: @MainActor () -> Session?, operation: @MainActor (Session) async throws -> Result) async throws -> Result {
        guard current() === expected, !Task.isCancelled else { throw CancellationError() }
        return try await operation(expected)
    }
}
