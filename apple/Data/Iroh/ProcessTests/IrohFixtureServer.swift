import AlondraTestSupport
import Foundation

/// The real `alondrad` serving a paired Mac on loopback over iroh. Its temporary tree lives in a
/// test-owned directory, and closing stdin stops it.
final class IrohFixtureServer: Sendable {
    struct Event: Decodable, Sendable {
        let event: String
        let endpoint: String?
        let address: String?
        let action: String?
        let limit: String?
    }

    private let process: Process
    private let input: FileHandle
    private let lines: AsyncThrowingStream<Event, any Error>

    init() throws {
        let environment = ProcessInfo.processInfo.environment
        guard let root = environment["TEST_SRCDIR"] else { throw FixtureError.missingRunfile }
        let executable = URL(fileURLWithPath: root).appending(path: environment["TEST_WORKSPACE"] ?? "_main")
            .appending(path: "crates/alondrad/alondrad_test")
        let home = try TestFiles.directory()
        let stdin = Pipe(), stdout = Pipe()
        process = Process()
        process.executableURL = executable
        process.arguments = ["--ignored", "--exact", "service::tests::iroh_tests::iroh_process_fixture_server", "--nocapture"]
        var childEnvironment = environment
        childEnvironment["HOME"] = home.path
        childEnvironment["TMPDIR"] = home.path
        process.environment = childEnvironment
        process.standardInput = stdin
        process.standardOutput = stdout
        process.standardError = FileHandle.nullDevice
        input = stdin.fileHandleForWriting
        let output = stdout.fileHandleForReading
        lines = AsyncThrowingStream { continuation in
            let reading = Task {
                do {
                    for try await line in output.bytes.lines where line.hasPrefix("ALONDRA_IROH ") {
                        continuation.yield(try JSONDecoder().decode(Event.self, from: Data(line.dropFirst(13).utf8)))
                    }
                    continuation.finish()
                } catch { continuation.finish(throwing: error) }
            }
            continuation.onTermination = { _ in reading.cancel() }
        }
        try process.run()
    }

    func next() async throws -> Event {
        for try await event in lines { return event }
        throw FixtureError.exited
    }

    func send(_ command: String) throws {
        try input.write(contentsOf: Data((command + "\n").utf8))
    }

    func stop() {
        try? input.close()
        process.waitUntilExit()
    }

    enum FixtureError: Error { case missingRunfile, exited }
}
