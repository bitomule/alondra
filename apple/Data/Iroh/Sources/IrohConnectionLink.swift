import AlondraTransport
import Foundation
import IrohLib

final class IrohConnectionLink: IrohLink {
    private let connection: Connection

    init(_ connection: Connection) {
        self.connection = connection
    }

    func exchange(_ request: Data, answerLimit: Int) async throws -> Data {
        let stream: BiStream
        do { stream = try await connection.openBi() } catch { throw IrohLinkError.notSent }
        let send = stream.send()
        do { try await send.writeAll(buf: request) } catch { throw IrohLinkError.notSent }
        do {
            try await send.finish()
            return try await stream.recv().readToEnd(sizeLimit: UInt32(answerLimit))
        } catch { throw IrohLinkError.noAnswer }
    }

    func path() -> IrohPath { Self.path(connection.paths()) }

    /// Polled: IrohLib 1.1.0's `watchPaths` panics when called outside its Tokio runtime.
    func pathChanges() -> AsyncStream<IrohPath> {
        let connection = connection
        return AsyncStream { continuation in
            let polling = Task {
                var last = Self.path(connection.paths())
                while !Task.isCancelled, connection.closeReason() == nil {
                    try? await Task.sleep(for: Self.pathPoll)
                    let now = Self.path(connection.paths())
                    if now != last {
                        last = now
                        continuation.yield(now)
                    }
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in polling.cancel() }
        }
    }

    static let pathPoll = Duration.milliseconds(500)

    var isClosed: Bool { connection.closeReason() != nil }

    func close() async {
        try? connection.close(errorCode: 0, reason: Data("closed".utf8))
    }

    static func path(_ paths: [PathSnapshot]) -> IrohPath {
        guard let selected = paths.first(where: \.isSelected) else { return .none }
        if selected.isIp { return .direct }
        return selected.isRelay ? .relay : .none
    }
}
