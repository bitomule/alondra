import Foundation

/// Builds an `AuthSession.RefreshGrant` from a platform background-time API. Every grant that
/// begins ends exactly once: when the refresh finishes or when the system expires it, whichever
/// comes first. Expiry also tells the session to cancel the refresh still on the wire.
public func pairedRefreshGrant<ID: Sendable>(
    begin: @escaping @MainActor @Sendable (_ expiration: @escaping @MainActor @Sendable () -> Void) -> ID?,
    end: @escaping @MainActor @Sendable (ID) -> Void
) -> AuthSession.RefreshGrant {
    { expired in
        let grant = await MainActor.run {
            let grant = PairedGrant(end: end)
            grant.start(begin({
                expired()
                grant.finish()
            }))
            return grant
        }
        return { await grant.finish() }
    }
}

@MainActor private final class PairedGrant<ID: Sendable> {
    private let end: @MainActor @Sendable (ID) -> Void
    private var id: ID?
    private var finished = false

    init(end: @escaping @MainActor @Sendable (ID) -> Void) { self.end = end }

    func start(_ id: ID?) {
        guard !finished else {
            if let id { end(id) }
            return
        }
        self.id = id
    }

    func finish() {
        finished = true
        guard let id else { return }
        self.id = nil
        end(id)
    }
}
