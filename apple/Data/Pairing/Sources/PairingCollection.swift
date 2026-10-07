import AlondraTransport
import Foundation

/// A local binding identity, not authorization. OIDC issuer is not stored in the legacy
/// pairing contract; callers must still validate the configured provider and owner.
public struct ApprovedPairingID: Codable, Hashable, Sendable {
    public let relayURL: URL?
    public let ownerSub: String?
    public let macID: String
    public let deviceID: String

    public init(_ pairing: ApprovedServicePairing) {
        relayURL = pairing.relayURL
        ownerSub = pairing.ownerSub
        macID = pairing.macID
        deviceID = pairing.deviceID
    }
}

/// Versioned storage for approved bindings. Selection remains single-active until
/// notification keys and push bindings support routing by individual binding.
public struct ApprovedPairingCollection: Codable, Sendable {
    public private(set) var pairings: [ApprovedServicePairing]
    public private(set) var selectedID: ApprovedPairingID?

    public init() {
        pairings = []
        selectedID = nil
    }

    public var selected: ApprovedServicePairing? {
        pairings.first { ApprovedPairingID($0) == selectedID }
    }

    /// Exact account/relay lookup. This does not claim isolation between OIDC issuers.
    public func pairings(ownerSub: String, relayURL: URL) -> [ApprovedServicePairing] {
        pairings.filter { $0.ownerSub == ownerSub && $0.relayURL == relayURL }
    }

    /// New approvals retain every other binding. Pinned Mac keys may only change
    /// after explicit removal; repeated saves never change a journal's revision.
    public mutating func upsert(_ pairing: ApprovedServicePairing) throws {
        let id = ApprovedPairingID(pairing)
        // A Mac paired without an account is pinned against every saved pairing of that Mac.
        for existing in pairings where existing.macID == pairing.macID && (!pairing.hasRelay || !existing.hasRelay
            || existing.relayURL == pairing.relayURL && existing.ownerSub == pairing.ownerSub) {
            guard existing.macSigningKey == pairing.macSigningKey,
                  existing.macAgreementKey == pairing.macAgreementKey else {
                throw PairingStoreError.identityConflict
            }
        }
        if let existing = pairings.first(where: { ApprovedPairingID($0) == id }) {
            guard existing.signingPrivateKey == pairing.signingPrivateKey,
                  existing.agreementPrivateKey == pairing.agreementPrivateKey else {
                throw PairingStoreError.identityConflict
            }
            // Keep the original approvedAtMs: it contributes to durable journal scope.
        } else {
            pairings.append(pairing)
        }
        selectedID = id
    }

    /// Keeps the addresses a Mac answers on directly. Keys and identity never change here.
    public mutating func updateRoutes(_ id: ApprovedPairingID, routes: [DirectEndpoint], name: String?) -> Bool {
        guard let index = pairings.firstIndex(where: { ApprovedPairingID($0) == id }) else { return false }
        let known = pairings[index].routes ?? []
        let merged = Array((known + routes.filter { !known.contains($0) }).suffix(12))
        let renamed = name.map { !$0.isEmpty && $0 != pairings[index].name } ?? false
        guard merged != known || renamed else { return false }
        pairings[index].routes = merged
        if renamed { pairings[index].name = name }
        return true
    }

    public mutating func remove(_ id: ApprovedPairingID) {
        pairings.removeAll { ApprovedPairingID($0) == id }
        if selectedID == id { selectedID = pairings.first.map(ApprovedPairingID.init) }
    }

    /// Signing out of the account removes only the pairings that depend on it.
    public mutating func removeRelayPairings() {
        pairings.removeAll(where: \.hasRelay)
        if let selectedID, !pairings.contains(where: { ApprovedPairingID($0) == selectedID }) {
            self.selectedID = pairings.first.map(ApprovedPairingID.init)
        }
    }

    public mutating func select(_ id: ApprovedPairingID) throws {
        guard pairings.contains(where: { ApprovedPairingID($0) == id }) else {
            throw PairingStoreError.invalidCollection
        }
        selectedID = id
    }

    private enum CodingKeys: String, CodingKey { case version, pairings, selectedID }

    public init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        if !values.contains(.version) {
            // Decode old bytes in memory only. A failed read must never rewrite keys.
            let legacy = try ApprovedServicePairing(from: decoder)
            pairings = [legacy]
            selectedID = ApprovedPairingID(legacy)
            return
        }
        guard try values.decode(Int.self, forKey: .version) == 1 else {
            throw PairingStoreError.unsupportedVersion
        }
        pairings = try values.decode([ApprovedServicePairing].self, forKey: .pairings)
        selectedID = try values.decodeIfPresent(ApprovedPairingID.self, forKey: .selectedID)
        let ids = pairings.map(ApprovedPairingID.init)
        guard Set(ids).count == ids.count,
              pairings.isEmpty ? selectedID == nil : selectedID.map(ids.contains) == true else {
            throw PairingStoreError.invalidCollection
        }
        // Refuse contradictory pins even if the serialized IDs differ by device.
        var validated = ApprovedPairingCollection()
        for pairing in pairings { try validated.upsert(pairing) }
    }

    public func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(1, forKey: .version)
        try values.encode(pairings, forKey: .pairings)
        try values.encodeIfPresent(selectedID, forKey: .selectedID)
    }
}
