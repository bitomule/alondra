import AlondraContracts
import Foundation

extension AlondraAPIClient {
    /// The semantic command ID is also the durable operation identity. A retry must
    /// reuse the whole envelope; callers must not invent a fresh ID after uncertainty.
    public func executeDomainCommand(_ envelope: DomainV2.CommandEnvelope) async throws -> DomainV2.CommandOutcome {
        guard envelope.protocolVersion == DomainV2.protocolVersion else {
            throw DomainV2.DomainError(code: "incompatible_protocol", message: "Unsupported domain protocol", currentRevision: nil, subject: nil)
        }
        guard envelope.expectedRevision <= DomainV2.maximumRevision,
              !envelope.commandId.rawValue.isEmpty, !envelope.dataEpoch.rawValue.isEmpty else {
            throw DomainV2.DomainError(code: "invalid_input", message: "Invalid command identity or revision", currentRevision: nil, subject: nil)
        }
        let endpoint = Endpoint<DomainV2.CommandOutcome>(
            .post, path: ["v2", "domain", "commands"],
            body: try DomainV2.encode(envelope), contentType: "application/json",
            requiresJSONObjectResponse: true
        ) { try DomainV2.decode(DomainV2.CommandOutcome.self, from: $0) }
        return try await send(endpoint, operationID: envelope.commandId.rawValue)
    }
}

extension AlondraAPIClient {
    public func domainCapabilities() async throws -> DomainV2.Capabilities {
        let endpoint = Endpoint<DomainV2.Capabilities>(.get, path: ["v2", "domain", "capabilities"]) {
            try DomainV2.decode(DomainV2.Capabilities.self, from: $0)
        }
        let capabilities = try await send(endpoint)
        guard capabilities.protocolVersion == DomainV2.protocolVersion else {
            throw DomainV2.DomainError(code: "incompatible_protocol", message: "Unsupported domain protocol", currentRevision: capabilities.revision, subject: nil)
        }
        return capabilities
    }
}

extension AlondraAPIClient {
    public func domainProjection() async throws -> DomainV2.DomainProjection {
        let endpoint = Endpoint<DomainV2.DomainProjection>(.get, path: ["v2", "domain"]) {
            try DomainV2.decode(DomainV2.DomainProjection.self, from: $0)
        }
        let projection = try await send(endpoint)
        guard projection.protocolVersion == DomainV2.protocolVersion,
              projection.revision <= DomainV2.maximumRevision else {
            throw DomainV2.DomainError(code: "incompatible_protocol", message: "Unsupported domain snapshot", currentRevision: nil, subject: nil)
        }
        return projection
    }
}

extension AlondraAPIClient {
    public func domainChanges(epoch: DomainV2.DataEpoch, after revision: DomainV2.Revision, wait seconds: UInt = 50) async throws -> DomainV2.Changes {
        let endpoint = Endpoint<DomainV2.Changes>(.get, path: ["v2", "domain", "changes"],
            query: [("data_epoch", epoch.rawValue), ("after", String(revision)), ("wait", String(min(seconds, 50)))]) {
                try DomainV2.decode(DomainV2.Changes.self, from: $0)
            }
        let changes = try await send(endpoint)
        guard changes.protocolVersion == DomainV2.protocolVersion,
              changes.dataEpoch == epoch, changes.revision >= revision,
              changes.revision <= DomainV2.maximumRevision else {
            throw DomainV2.DomainError(code: "incompatible_protocol", message: "Unsupported domain changes", currentRevision: nil, subject: nil)
        }
        return changes
    }
}
