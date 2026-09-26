import Foundation

/// Process-owned sessions for one verified account. Callers validate the relay allowlist
/// before passing pairings. A stored subject is not authentication by itself.
@MainActor
public final class ApprovedSessionRegistry<Session: AnyObject> {
    public struct Entry {
        public let approved: ApprovedServicePairing
        public let session: Session
        public var id: ApprovedPairingID { ApprovedPairingID(approved) }
    }
    public struct Snapshot {
        public let entries: [Entry]
        public let selectedID: ApprovedPairingID?
        public var selected: Entry? { entries.first { $0.id == selectedID } }
    }
    private let make: @MainActor (ApprovedServicePairing) throws -> Session
    private let stop: @MainActor (Session) async -> Void
    // Preparation may suspend, but registry and platform publication share one
    // synchronous MainActor commit after the generation/cancellation guards.
    private let publish: @MainActor (Snapshot) async throws -> (@MainActor () -> Void)
    private let clearPlatform: @MainActor () async throws -> Void
    private var generation = 0
    private var transition: UUID?
    private var owner: String?
    private var entries: [ApprovedPairingID: Entry] = [:]
    private var pending: [ApprovedPairingID: Entry] = [:]
    private var selectedID: ApprovedPairingID?
    public var snapshot: Snapshot { Snapshot(entries: Array(entries.values), selectedID: selectedID) }

    public init(make: @escaping @MainActor (ApprovedServicePairing) throws -> Session,
                stop: @escaping @MainActor (Session) async -> Void,
                publish: @escaping @MainActor (Snapshot) async throws -> (@MainActor () -> Void),
                clearPlatform: @escaping @MainActor () async throws -> Void) {
        self.make = make; self.stop = stop; self.publish = publish; self.clearPlatform = clearPlatform
    }

    /// Same-account reconciliation retains sessions; another account first loses every
    /// previous capability. The caller provides a subject freshly verified by OIDC.
    public func reconcile(_ pairings: [ApprovedServicePairing], verifiedSubject: String,
                          preferred: ApprovedPairingID?) async throws {
        if let owner, owner != verifiedSubject {
            let clearedGeneration = generation + 1
            try await clear()
            guard generation == clearedGeneration, !Task.isCancelled else { throw CancellationError() }
        }
        guard transition == nil else { throw CancellationError() }
        let operation = UUID()
        transition = operation
        defer { if transition == operation { transition = nil } }
        generation += 1
        let current = generation
        let authorized = pairings.filter { $0.ownerSub == verifiedSubject }
        guard !verifiedSubject.isEmpty, Set(authorized.map(ApprovedPairingID.init)).count == authorized.count else {
            throw PairingStoreError.invalidCollection
        }
        var next: [ApprovedPairingID: Entry] = [:]
        var created: [Entry] = []
        do {
            for approved in authorized {
                let id = ApprovedPairingID(approved)
                if let retained = entries[id] {
                    // Never reuse a live capability for silently changed key material.
                    guard retained.approved.signingPrivateKey == approved.signingPrivateKey,
                          retained.approved.agreementPrivateKey == approved.agreementPrivateKey,
                          retained.approved.macSigningKey == approved.macSigningKey,
                          retained.approved.macAgreementKey == approved.macAgreementKey else {
                        throw PairingStoreError.identityConflict
                    }
                    next[id] = retained
                } else {
                    let entry = Entry(approved: approved, session: try make(approved))
                    next[id] = entry
                    pending[id] = entry
                    created.append(entry)
                }
            }
            let selection = preferred.flatMap { next[$0] == nil ? nil : $0 }
                ?? selectedID.flatMap { next[$0] == nil ? nil : $0 }
                ?? authorized.first.map(ApprovedPairingID.init)
            let commit = try await publish(Snapshot(entries: authorized.compactMap { next[ApprovedPairingID($0)] }, selectedID: selection))
            guard current == generation, !Task.isCancelled else { throw CancellationError() }
            let removed = entries.filter { next[$0.key] == nil }.map(\.value)
            commit()
            entries = next; pending = [:]; selectedID = selection; owner = verifiedSubject
            for entry in removed { await stop(entry.session) }
        } catch {
            // clear() may already have stopped these; stop is intentionally idempotent.
            for entry in created {
                if pending[entry.id]?.session === entry.session { pending.removeValue(forKey: entry.id) }
                await stop(entry.session)
            }
            throw error
        }
    }

    /// Selection changes neither credentials nor the life of another approved session.
    public func select(_ id: ApprovedPairingID, verifiedSubject: String) async throws {
        guard owner == verifiedSubject, id.ownerSub == verifiedSubject, entries[id] != nil else {
            throw PairingStoreError.invalidCollection
        }
        guard transition == nil else { throw CancellationError() }
        let operation = UUID()
        transition = operation
        defer { if transition == operation { transition = nil } }
        generation += 1
        let current = generation
        let commit = try await publish(Snapshot(entries: Array(entries.values), selectedID: id))
        guard current == generation, !Task.isCancelled else { throw CancellationError() }
        commit()
        selectedID = id
    }

    /// Logout/account replacement invalidates visible and in-flight capabilities before
    /// any suspension. Every session is stopped even if platform cleanup throws.
    public func clear() async throws {
        generation += 1
        transition = nil
        let all = Array(entries.values) + Array(pending.values)
        entries = [:]; pending = [:]; selectedID = nil; owner = nil
        var failure: (any Error)?
        do { try await clearPlatform() } catch { failure = error }
        for entry in all { await stop(entry.session) }
        if let failure { throw failure }
    }
}
