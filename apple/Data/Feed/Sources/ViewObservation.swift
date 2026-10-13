import AlondraContract
import Foundation

enum ViewObservation {
    /// Subscribes before opening the key so a change between the two cannot be missed, sends the
    /// known version with every refresh so an unchanged view costs nothing, and releases the key
    /// when the consumer stops. A refresh that fails for any reason other than the key going away
    /// is retried on the next change.
    static func stream(session: FeedSession, key: ViewKey) -> AsyncThrowingStream<ViewFetch, any Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                let events = await session.events()
                let lease: ViewLease
                var version: String
                do {
                    let opened = try await session.open(key.rawValue)
                    guard let document = opened.document else {
                        await session.close(opened.lease)
                        continuation.finish(throwing: ViewsClientError.missingView)
                        return
                    }
                    lease = opened.lease
                    version = document.version
                    continuation.yield(.document(document))
                } catch {
                    continuation.finish(throwing: error)
                    return
                }
                var failure: (any Error)?
                for await event in events {
                    let known: String?
                    switch event {
                    case .stale(let keys):
                        guard keys.contains(key.rawValue) else { continue }
                        known = version
                    case .reset:
                        known = nil
                    case .snapshot:
                        continue
                    }
                    do {
                        if case .document(let document) = try await session.load(key.rawValue, known: known, priority: .background) {
                            version = document.version
                            continuation.yield(.document(document))
                        }
                    } catch FeedError.missingView {
                        failure = ViewsClientError.missingView
                        break
                    } catch is CancellationError {
                        break
                    } catch {
                        continue
                    }
                }
                await session.close(lease)
                continuation.finish(throwing: failure)
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
}
