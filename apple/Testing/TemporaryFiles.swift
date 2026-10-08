import Foundation
import Testing

public enum TestFiles {
    @TaskLocal static var root: URL?

    public static func directory() throws -> URL {
        guard let root else { throw MissingScope() }
        let directory = root.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory
    }

    private struct MissingScope: Error {}
}

public struct TemporaryFiles: TestTrait, SuiteTrait, TestScoping {
    public init() {}
    public var isRecursive: Bool { true }

    public func scopeProvider(for test: Test, testCase: Test.Case?) -> Self? {
        test.isSuite ? nil : self
    }

    public func provideScope(for test: Test, testCase: Test.Case?,
                             performing operation: @Sendable () async throws -> Void) async throws {
        let base = ProcessInfo.processInfo.environment["TEST_TMPDIR"].map {
            URL(fileURLWithPath: $0, isDirectory: true)
        } ?? FileManager.default.temporaryDirectory
        let directory = base.appendingPathComponent("alondra-test-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer {
            do { try FileManager.default.removeItem(at: directory) }
            catch { Issue.record(error, "Failed to remove test files at \(directory.path)") }
        }
        try await TestFiles.$root.withValue(directory, operation: operation)
    }
}

extension Trait where Self == TemporaryFiles {
    public static var temporaryFiles: Self { .init() }
}
