import Foundation
import Synchronization
import Testing

public enum TestFiles {
    @TaskLocal static var scope: Scope?

    public static func directory() throws -> URL {
        guard let scope else { throw MissingScope() }
        let directory = scope.root.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory
    }

    /// Stops something that writes to these files: it runs when the test ends, thrown or not,
    /// and is awaited before the files are removed. The last one added runs first.
    public static func beforeRemoval(_ stop: @escaping @Sendable () async throws -> Void) throws {
        guard let scope else { throw MissingScope() }
        scope.stops.withLock { $0.append(stop) }
    }

    final class Scope: Sendable {
        let root: URL
        let stops = Mutex<[@Sendable () async throws -> Void]>([])
        init(root: URL) { self.root = root }

        func stopAll() async {
            for stop in stops.withLock({ $0 }).reversed() {
                do { try await stop() }
                catch { Issue.record(error, "Failed to stop a writer before removing test files at \(root.path)") }
            }
        }
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
        let scope = TestFiles.Scope(root: directory)
        do {
            try await TestFiles.$scope.withValue(scope) { try await operation() }
        } catch {
            await scope.stopAll()
            throw error
        }
        await scope.stopAll()
    }
}

extension Trait where Self == TemporaryFiles {
    public static var temporaryFiles: Self { .init() }
}
