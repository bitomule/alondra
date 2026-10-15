import AlondraTestSupport
import Foundation
import Synchronization
import Testing

private struct FailedBody: Error {}

@Test(arguments: [false, true])
func scopedFilesAreRemovedOnReturnAndThrow(throwsFromBody: Bool) async throws {
    let written = Mutex<URL?>(nil)
    let test = try #require(Test.current)
    do {
        try await TemporaryFiles().provideScope(for: test, testCase: nil) {
            let directory = try TestFiles.directory()
            written.withLock { $0 = directory }
            try Data("test fixture".utf8).write(to: directory.appendingPathComponent("fixture"))
            if throwsFromBody { throw FailedBody() }
        }
        #expect(!throwsFromBody)
    } catch is FailedBody {
        #expect(throwsFromBody)
    }
    let directory = try #require(written.withLock { $0 })
    #expect(!FileManager.default.fileExists(atPath: directory.path))
    #expect(!FileManager.default.fileExists(atPath: directory.deletingLastPathComponent().path))
}

@Suite(.temporaryFiles)
struct TemporaryFilesScopeTests {
    @Test func callsReceiveDifferentDirectories() throws {
        let first = try TestFiles.directory()
        let second = try TestFiles.directory()
        #expect(first != second)
        #expect(FileManager.default.fileExists(atPath: first.path))
        #expect(FileManager.default.fileExists(atPath: second.path))
    }
}

@Test(arguments: [false, true])
func writersStopNewestFirstBeforeTheFilesAreRemoved(throwsFromBody: Bool) async throws {
    let stopped = Mutex<[String]>([])
    let test = try #require(Test.current)
    do {
        try await TemporaryFiles().provideScope(for: test, testCase: nil) {
            let directory = try TestFiles.directory()
            for writer in ["first", "second"] {
                try TestFiles.beforeRemoval {
                    let filesRemain = FileManager.default.fileExists(atPath: directory.path)
                    stopped.withLock { $0.append("\(writer) \(filesRemain)") }
                }
            }
            if throwsFromBody { throw FailedBody() }
        }
        #expect(!throwsFromBody)
    } catch is FailedBody {
        #expect(throwsFromBody)
    }
    #expect(stopped.withLock { $0 } == ["second true", "first true"])
}
